use std::future::Future;
use std::ops::Deref;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use crate::{MaybeSend, MaybeSync, XRc};
use crate::taskmgr::{ReturnTracker, Hooks};

pub(crate) struct ThreadData {
    cancel_token: CancellationToken,
    pub(crate) return_tracker: ReturnTracker,
}

impl ThreadData {
    /// Returns the underlying thread data if it exists
    pub fn get_existing(th: &mluau::Thread) -> Option<XRc<ThreadData>> {
        th.thread_data::<ThreadData>()
    }

    /// Gets the ThreadData, creating it if it doesn't exist
    pub fn get<R>(th: &mluau::Thread, f: impl FnOnce(&ThreadData) -> R) -> R {
        match th.thread_data::<ThreadData>() {
            Some(data) => (f)(&data),
            None => {
                let token = CancellationToken::new();
                let data = ThreadData {
                    cancel_token: token,
                    return_tracker: ReturnTracker::new(),
                };
                let r = (f)(&data);
                let _ = th.set_thread_data(data).expect("internal error: failed to set thread data");
                r
            },
        }
    }
}

pub struct CoreSchedulerInnerV3 {
    lua: mluau::WeakLua,
    hooks: XRc<dyn Hooks>,
    cancel_token: CancellationToken,
    tracker: TaskTracker,
}

#[derive(Clone)]
pub struct CoreSchedulerV3 {
    inner: XRc<CoreSchedulerInnerV3>,
}

impl Deref for CoreSchedulerV3 {
    type Target = CoreSchedulerInnerV3;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl CoreSchedulerV3 {
    pub fn new(
        lua: mluau::WeakLua,
        hooks: XRc<dyn Hooks>,
    ) -> Self {
        Self {
            inner: XRc::new(CoreSchedulerInnerV3 {
                lua,
                hooks,
                cancel_token: CancellationToken::new(),
                tracker: TaskTracker::new(),
            })
        }
    }

    /// Tries to get strong ref to lua
    pub fn get_lua(&self) -> Option<mluau::Lua> {
        self.lua.try_upgrade()
    }

    /// Resumes a Lua thread with the given arguments, handling cancellation and Lua state validity
    /// If args is Ok(T), resumes with values T; if Err(E), resumes with error E
    fn resume_thread(&self, thread: mluau::Thread, args: Result<impl mluau::IntoLuaMulti, impl mluau::IntoLua>) {
        if self.lua.strong_count() == 0 { 
            return; 
        }

        if matches!(thread.status(), mluau::ThreadStatus::Error | mluau::ThreadStatus::Finished) {
            return;
        }

        self.hooks.on_resume(&thread);
        if let Some(data) = ThreadData::get_existing(&thread) && data.return_tracker.is_tracking() {
            let result = match args {
                Ok(v) => thread.resume(v),
                Err(e) => thread.resume_error(e),
            };
            let _ = data.return_tracker.push_result(result);
        } else {
            // fast-path: we aren't tracking this thread, so no need to store the result
            let _ = match args {
                Ok(v) => thread.resume::<()>(v),
                Err(e) => thread.resume_error::<()>(e),
            };
        }
    }

    #[cfg(not(feature = "send"))]
    fn spawn<F>(&self, task: F) -> JoinHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: 'static,
        {
        self.tracker.spawn_local(task)
    }

    #[cfg(feature = "send")]
    fn spawn<F>(&self, task: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tracker.spawn(task)
    }

    pub fn schedule_wait(&self, thread: mluau::Thread, duration: Duration) {
        let this = self.clone(); // Cheap Rc clone
        let start_at = Instant::now();
        let cancel_token = ThreadData::get(&thread, |data| data.cancel_token.clone());         
        self.spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep_until(start_at + duration) => {
                    this.resume_thread(thread, Ok::<_, mluau::Error>(start_at.elapsed().as_secs_f64())); 
                }
                _ = cancel_token.cancelled() => {
                    // Task was cancelled, do nothing
                }
            }
        });
    }

    pub fn schedule_deferred(&self, thread: mluau::Thread, args: mluau::MultiValue) {
        let this = self.clone();
        let cancel_token = ThreadData::get(&thread, |data| data.cancel_token.clone());         
        self.spawn(async move {
            tokio::select! {
                _ = tokio::task::yield_now() => {
                    this.resume_thread(thread, Ok::<_, mluau::Error>(args)); 
                }
                _ = cancel_token.cancelled() => {
                    // Task was cancelled, do nothing
                }
            }
        });
    }

    pub fn schedule_delay(&self, thread: mluau::Thread, duration: Duration, args: mluau::MultiValue) {
        let this = self.clone(); // Cheap Rc clone
        let start_at = Instant::now();
        let cancel_token = ThreadData::get(&thread, |data| data.cancel_token.clone());         
        self.spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep_until(start_at + duration) => {
                    this.resume_thread(thread, Ok::<_, mluau::Error>(args)); 
                }
                _ = cancel_token.cancelled() => {
                    // Task was cancelled, do nothing
                }
            }
        });
    }

    pub fn schedule_async<F>(&self, thread: mluau::Thread, fut: F) 
    where 
        F: Future<Output = mluau::Result<mluau::MultiValue>> + MaybeSend + MaybeSync + 'static 
    {
        let fut = std::panic::AssertUnwindSafe(fut).catch_unwind();
        let this = self.clone(); // Cheap Rc clone
        let cancel_token = ThreadData::get(&thread, |data| data.cancel_token.clone());         
        self.spawn(async move {
            tokio::select! {
                res = fut => {
                    match res {
                        Ok(val) => this.resume_thread(thread, val),
                        Err(err) => {
                            let mut error_payload = format!("Error in async thread: {err:?}");
                            if let Some(error) = err.downcast_ref::<String>() {
                                error_payload = format!("Error in async thread: {error}");
                            }
                            if let Some(error) = err.downcast_ref::<&str>() {
                                error_payload = format!("Error in async thread: {error}");
                            }

                            this.resume_thread(thread, Err::<(), String>(error_payload));
                        },
                    }
                }
                _ = cancel_token.cancelled() => {
                    // Task was cancelled, do nothing
                }
            }
        });
    }

    pub fn cancel_thread(&self, thread: &mluau::Thread) -> bool {
        match ThreadData::get_existing(thread) {
            Some(data) => {
                data.cancel_token.cancel();
                true
            },
            None => false,
        }
    }

    pub fn stop(&self) {
        self.tracker.close();
        self.cancel_token.cancel();
    }

    pub async fn wait_till_done(&self) {
        self.tracker.close();
        self.tracker.wait().await; // Wait for all running tasks to finish
    }
}