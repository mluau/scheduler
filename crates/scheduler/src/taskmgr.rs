use crate::{XId, XRc, XRefCell};
use std::collections::HashMap;

#[allow(clippy::type_complexity)]
pub struct ReturnTracker {
    inner: XRc<
        XRefCell<HashMap<XId, tokio::sync::mpsc::UnboundedSender<mluau::Result<mluau::MultiValue>>>>,
    >,
    wildcard: XRc<
        XRefCell<
            Option<
                tokio::sync::mpsc::UnboundedSender<(mluau::Thread, mluau::Result<mluau::MultiValue>)>,
            >,
        >,
    >,
}

impl Default for ReturnTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ReturnTracker {
    /// Creates a new return tracker
    pub fn new() -> Self {
        Self {
            inner: XRc::new(XRefCell::new(HashMap::new())),
            wildcard: XRc::new(XRefCell::new(None)),
        }
    }

    /// Track a threads result
    pub fn track_thread(
        &self,
        th: &mluau::Thread,
    ) -> tokio::sync::mpsc::UnboundedReceiver<mluau::Result<mluau::MultiValue>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.inner
            .borrow_mut()
            .insert(XId::from_ptr(th.to_pointer()), tx);

        rx
    }

    pub fn track_wildcard_thread(
        &self,
    ) -> tokio::sync::mpsc::UnboundedReceiver<(mluau::Thread, mluau::Result<mluau::MultiValue>)> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut inner = self.wildcard.borrow_mut();
        *inner = Some(tx);
        rx
    }

    /// Set a wildcard sender for all threads
    pub fn set_wildcard_sender(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<(mluau::Thread, mluau::Result<mluau::MultiValue>)>,
    ) {
        let mut inner = self.wildcard.borrow_mut();
        *inner = Some(sender);
    }

    /// Push a result to the tracked thread
    pub fn push_result(&self, th: &mluau::Thread, result: mluau::Result<mluau::MultiValue>) {
        log::trace!("ThreadTracker: Pushing result to thread {th:?}");

        {
            let inner = self.wildcard.borrow();
            if let Some(ref tx) = *inner {
                // Remove the thread from the tracker
                let _ = tx.send((th.clone(), result.clone()));
            }
        }

        {
            let inner = self.inner.borrow();

            if let Some(tx) = inner.get(&XId::from_ptr(th.to_pointer())) {
                let _ = tx.send(result);
            }
        }
    }
}

#[derive(Clone)]
/// Task Manager
pub struct TaskManager {
    pub(crate) inner: XRc<crate::taskmgr_v2::CoreScheduler>,
}

impl std::ops::Deref for TaskManager {
    type Target = crate::taskmgr_v2::CoreScheduler;

    fn deref(&self) -> &Self::Target {
        self.inner.as_ref()
    }
}

impl TaskManager {
    /// Creates a new task manager
    pub fn new(lua: &mluau::Lua, returns: ReturnTracker) -> Self {
        Self {
            inner: crate::taskmgr_v2::CoreScheduler::new(lua.weak(), returns).into(),
        }
    }

    /// Tries to get strong ref to lua
    pub fn get_lua(&self) -> Option<mluau::Lua> {
        self.inner.lua().try_upgrade()
    }

    /// Attaches the task manager to the lua state. Note that run_in_task (etc.) must also be called
    pub fn attach(&self) -> Result<(), mluau::Error> {
        let Some(lua) = self.get_lua() else {
            return Err(mluau::Error::RuntimeError(
                "Failed to upgrade lua".to_string(),
            ));
        };
        lua.set_app_data(self.clone());
        Ok(())
    }

    /// Returns whether the task manager has been cancelled
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// Returns whether the task manager is running
    pub fn is_running(&self) -> bool {
        self.inner.is_running()
    }

    /// Returns the returns stored in the task manager
    pub fn returns(&self) -> &ReturnTracker {
        self.inner.returns()
    }

    /// Adds a waiting thread to the task manager
    #[inline]
    pub fn add_waiting_thread(
        &self,
        thread: mluau::Thread,
        delay_args: Option<mluau::MultiValue>,
        duration: std::time::Duration,
    ) {
        self.inner
            .push_event(crate::taskmgr_v2::SchedulerEvent::Wait {
                delay_args,
                thread,
                duration,
                start_at: std::time::Instant::now(),
            });
    }

    /// Cancels a thread that is waiting in the task manager
    #[inline]
    pub fn cancel_task(&self, thread: &mluau::Thread) {
        self.inner
            .cancel_task(crate::XId::from_ptr(thread.to_pointer()));
    }

    /// Adds a deferred thread to the task manager to the front of the queue
    #[inline]
    pub fn add_deferred_thread(&self, thread: mluau::Thread, args: mluau::MultiValue) {
        self.inner
            .push_event(crate::taskmgr_v2::SchedulerEvent::DeferredThread { args, thread });
    }

    pub fn run_in_task(&self) {
        if self.is_running() || self.is_cancelled() || !self.check_lua() {
            return;
        }

        log::debug!("Firing up task manager");

        let self_ref = self.clone();

        #[cfg(feature = "send")]
        tokio::task::spawn(async move {
            self_ref.run().await;
        });
        #[cfg(not(feature = "send"))]
        tokio::task::spawn_local(async move {
            self_ref.run().await;
        });
    }

    /// Checks if the lua state is valid
    fn check_lua(&self) -> bool {
        self.inner.lua().try_upgrade().is_some()
    }

    /// Stops the task manager
    pub fn stop(&self) {
        self.set_cancelled(true);
    }

    /// Unstops the task manager
    pub fn unstop(&self) {
        self.set_cancelled(false);
    }

    /// Clears the task manager queues completely
    pub fn clear(&self) {
        self.inner
            .push_event(crate::taskmgr_v2::SchedulerEvent::Clear {});
    }

    /// Spawns a thread, discarding its output entirely
    pub async fn spawn_thread(&self, thread: mluau::Thread, args: mluau::MultiValue) {
        let resp = thread.resume(args);

        self.returns().push_result(&thread, resp);
    }

    /// Spawns a thread and then proceeds to get its output properly
    ///
    /// This requires ThreadTracker to be attached to the scheduler
    pub async fn spawn_thread_and_wait(
        &self,
        thread: mluau::Thread,
        args: mluau::MultiValue,
    ) -> Result<Option<mluau::Result<mluau::MultiValue>>, mluau::Error> {
        let mut rx = self.returns().track_thread(&thread);

        let result = thread.resume(args);

        self.returns().push_result(&thread, result);

        let mut value: Option<mluau::Result<mluau::MultiValue>> = None;

        loop {
            let Some(next) = rx.recv().await else {
                break;
            };

            if self.get_lua().is_none() {
                log::trace!("Scheduler is no longer valid, exiting...");
                break;
            }

            log::trace!("Received value: {next:?}");
            value = Some(next);

            let status = thread.status();
            if (status == mluau::ThreadStatus::Finished || status == mluau::ThreadStatus::Error)
                && rx.is_empty()
            {
                log::trace!("Status: {status:?}");
                break;
            }
        }

        Ok(value)
    }
}

pub fn get(lua: &'_ mluau::Lua) -> mluau::AppDataRef<'_, TaskManager> {
    lua.app_data_ref::<TaskManager>()
        .expect("Failed to get task manager")
}
