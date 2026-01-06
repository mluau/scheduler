use std::future::Future;
use std::ops::Deref;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::MaybeSend;
use crate::MaybeSync;
use crate::taskmgr::ReturnTracker;
use crate::taskmgr::Hooks;
use crate::XRc;

pub struct ThreadData {
    cancel_token: CancellationToken,
}

pub struct CoreSchedulerInnerV3 {
    lua: mluau::WeakLua,
    returns: ReturnTracker,
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
        returns: ReturnTracker,
        hooks: XRc<dyn Hooks>,
    ) -> Self {
        Self {
            inner: XRc::new(CoreSchedulerInnerV3 {
                lua,
                returns,
                hooks,
                cancel_token: CancellationToken::new(),
                tracker: TaskTracker::new(),
            })
        }
    }

    /// Returns the ReturnTracker
    pub fn returns(&self) -> &ReturnTracker {
        &self.returns
    }

    /// Tries to get strong ref to lua
    pub fn get_lua(&self) -> Option<mluau::Lua> {
        self.lua.try_upgrade()
    }

    /// Resumes a Lua thread with the given arguments, handling cancellation and Lua state validity
    fn resume_thread_ok(&self, thread: mluau::Thread, args: impl mluau::IntoLuaMulti) {
        if self.cancel_token.is_cancelled() { 
            return; 
        }
        
        if self.lua.try_upgrade().is_none() { 
            return; 
        }

        if matches!(thread.status(), mluau::ThreadStatus::Error | mluau::ThreadStatus::Finished) {
            return;
        }

        self.hooks.on_resume(&thread);
        if self.returns.is_tracking(&thread) {
            let result = thread.resume(args);
            self.returns.push_result(&thread, result);
        } else {
            // fast-path: we aren't tracking this thread, so no need to store the result
            let _ = thread.resume::<()>(args);
        }

        //self.returns.push_result(&thread, result);
    }

    /// Resumes a Lua thread with an error, handling cancellation and Lua state validity
    fn resume_thread_err(&self, thread: mluau::Thread, err: impl mluau::IntoLua) {
        if self.cancel_token.is_cancelled() { 
            return; 
        }
        
        if self.lua.try_upgrade().is_none() { 
            return; 
        }

        if matches!(thread.status(), mluau::ThreadStatus::Error | mluau::ThreadStatus::Finished) {
            return;
        }

        self.hooks.on_resume(&thread);
        if self.returns.is_tracking(&thread) {
            let result = thread.resume_error(err);
            self.returns.push_result(&thread, result);
        } else {
            // fast-path: we aren't tracking this thread, so no need to store the result
            let _ = thread.resume_error::<()>(err);
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

    /// Helper method to return a cancellation token for the task associated with the given thread
    pub fn get_thread_token(&self, thread: &mluau::Thread) -> CancellationToken { 
        match thread.thread_data::<ThreadData>() {
            Some(data) => data.cancel_token.clone(),
            None => {
                let token = self.cancel_token.child_token();
                let data = ThreadData {
                    cancel_token: token.clone(),
                };
                let _ = thread.set_thread_data(data).expect("internal error: failed to set thread data");
                token
            },
        }
    }

    pub fn schedule_wait(&self, thread: mluau::Thread, duration: Duration) {
        let this = self.clone(); // Cheap Rc clone
        let start_at = Instant::now();
        let cancel_token = self.get_thread_token(&thread);         
        self.spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep_until(start_at + duration) => {
                    this.resume_thread_ok(thread, start_at.elapsed().as_secs_f64()); 
                }
                _ = cancel_token.cancelled() => {
                    // Task was cancelled, do nothing
                }
            }
        });
    }

    pub fn schedule_deferred(&self, thread: mluau::Thread, args: mluau::MultiValue) {
        let this = self.clone();
        let cancel_token = self.get_thread_token(&thread);
        self.spawn(async move {
            tokio::select! {
                _ = tokio::task::yield_now() => {
                    this.resume_thread_ok(thread, args); 
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
        let cancel_token = self.get_thread_token(&thread);
        self.spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep_until(start_at + duration) => {
                    this.resume_thread_ok(thread, args); 
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
        let cancel_token = self.get_thread_token(&thread);
        self.spawn(async move {
            tokio::select! {
                res = fut => {
                    match res {
                        Ok(Ok(vals)) => this.resume_thread_ok(thread, vals),
                        Ok(Err(err)) => this.resume_thread_err(thread, err),
                        Err(err) => {
                            let mut error_payload = format!("Error in async thread: {err:?}");
                            if let Some(error) = err.downcast_ref::<String>() {
                                error_payload = format!("Error in async thread: {error}");
                            }
                            if let Some(error) = err.downcast_ref::<&str>() {
                                error_payload = format!("Error in async thread: {error}");
                            }

                            this.resume_thread_err(thread, error_payload);
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
        match thread.thread_data::<ThreadData>() {
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