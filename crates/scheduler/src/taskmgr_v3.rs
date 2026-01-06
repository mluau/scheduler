use std::future::Future;
use std::ops::Deref;
use std::time::Duration;

use futures_util::FutureExt;
use rustc_hash::FxHashMap;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::MaybeSend;
use crate::MaybeSync;
use crate::XRefCell;
use crate::XU64;
use crate::XWeak;
use crate::taskmgr::ReturnTracker;
use crate::taskmgr::Hooks;
use crate::{XId, XRc};

/// A guard that removes a task from the tracking map when dropped
struct TaskGuard {
    thread_id: XId,
    op_id: u64,
    // Weak ref to avoids cycles
    tasks: XWeak<XRefCell<FxHashMap<XId, FxHashMap<u64, JoinHandle<()>>>>>,
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        //println!("Dropping TaskGuard for thread ID {:?}, op ID {}", self.thread_id, self.op_id);
        if let Some(tasks_rc) = self.tasks.upgrade() {
            let mut threads_map = tasks_rc.borrow_mut();
            
            // Find the operations map for this thread
            if let Some(ops_map) = threads_map.get_mut(&self.thread_id) {
                // Remove THIS specific operation
                ops_map.remove(&self.op_id);

                // Clean up the thread entry if it has no more running tasks
                if ops_map.is_empty() {
                    threads_map.remove(&self.thread_id);
                }
            }
        }
    }
}

pub struct CoreSchedulerInnerV3 {
    lua: mluau::WeakLua,
    returns: ReturnTracker,
    hooks: XRc<dyn Hooks>,
    cancel_token: CancellationToken,
    tracker: TaskTracker,

    // Used for thread cancellation
    tasks: XRc<XRefCell<FxHashMap<XId, FxHashMap<u64, JoinHandle<()>>>>>,
    next_op_id: XU64,
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
                tasks: XRc::new(XRefCell::new(FxHashMap::default())),
                next_op_id: XU64::new(0),
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

        let result = thread.resume(args);

        self.returns.push_result(&thread, result);
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
        let result = thread.resume_error(err);

        self.returns.push_result(&thread, result);
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

/// Internal helper to spawn and track a task securely
    fn spawn_tracked<F>(&self, thread_id: XId, fut: F)
    where
        F: std::future::Future<Output = ()> + MaybeSend + 'static,
    {        
        let op_id = self.inner.next_op_id.get();
        self.inner.next_op_id.set(op_id.wrapping_add(1));

        let guard = TaskGuard {
            thread_id: thread_id.clone(),
            op_id,
            tasks: XRc::downgrade(&self.inner.tasks),
        };

        let handle = self.spawn(async move {
            let _keep_alive = guard;
            fut.await;
        });

        // 4. Insert Handle
        self.inner.tasks.borrow_mut()
            .entry(thread_id)
            .or_default()
            .insert(op_id, handle);
    }

    pub fn schedule_wait(&self, thread: mluau::Thread, duration: Duration) {
        let this = self.clone(); // Cheap Rc clone
        let start_at = Instant::now();            
        self.spawn_tracked(XId::from_ptr(thread.to_pointer()), async move {
            tokio::select! {
                _ = tokio::time::sleep(duration) => {
                    this.resume_thread_ok(thread, start_at.elapsed().as_secs_f64()); 
                }
                _ = this.cancel_token.cancelled() => {
                    // Task was cancelled, do nothing
                }
            }
        });
    }

    pub fn schedule_deferred(&self, thread: mluau::Thread, args: mluau::MultiValue) {
        let this = self.clone();
        self.spawn_tracked(XId::from_ptr(thread.to_pointer()), async move {
            tokio::select! {
                _ = tokio::task::yield_now() => {
                    this.resume_thread_ok(thread, args); 
                }
                _ = this.cancel_token.cancelled() => {
                    // Task was cancelled, do nothing
                }
            }
        });
    }

    pub fn schedule_delay(&self, thread: mluau::Thread, duration: Duration, args: mluau::MultiValue) {
        let this = self.clone(); // Cheap Rc clone
        self.spawn_tracked(XId::from_ptr(thread.to_pointer()), async move {
            tokio::select! {
                _ = tokio::time::sleep(duration) => {
                    this.resume_thread_ok(thread, args); 
                }
                _ = this.cancel_token.cancelled() => {
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
        self.spawn_tracked(XId::from_ptr(thread.to_pointer()), async move {
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
                _ = this.cancel_token.cancelled() => {
                    // Task was cancelled, do nothing
                }
            }
        });
    }

    pub fn cancel_thread(&self, thread: &mluau::Thread) -> bool {
        let thread_id = XId::from_ptr(thread.to_pointer());

        if let Some(ops_map) = self.inner.tasks.borrow().get(&thread_id) {
            // Abort all handles. 
            // This triggers the task to drop at the next await point.
            // The drop triggers TaskGuard, which cleans up the map.
            for handle in ops_map.values() {
                handle.abort();
            }
            return true;
        }
        false
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