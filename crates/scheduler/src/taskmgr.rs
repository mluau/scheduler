use crate::{MaybeSend, MaybeSync};
use crate::{XId, XRc, XRefCell};
use std::collections::HashMap;
use std::future::Future;

#[allow(clippy::type_complexity)]
pub struct ReturnTracker {
    inner:
        XRefCell<
            HashMap<XId, tokio::sync::mpsc::UnboundedSender<mluau::Result<mluau::MultiValue>>>,
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
            inner: XRefCell::new(HashMap::new()),
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

    /// Returns if a thread is being tracked
    pub fn is_tracking(&self, th: &mluau::Thread) -> bool {
        self.inner
            .borrow()
            .contains_key(&XId::from_ptr(th.to_pointer()))
    }

    /// Push a result to the tracked thread
    pub fn push_result(&self, th: &mluau::Thread, result: mluau::Result<mluau::MultiValue>) {
        log::trace!("ThreadTracker: Pushing result to thread {th:?}");

        {
            let inner = self.inner.borrow();

            if let Some(tx) = inner.get(&XId::from_ptr(th.to_pointer())) {
                let _ = tx.send(result);
            }
        }
    }
}

type Error = Box<dyn std::error::Error + Send + Sync>;

pub trait Hooks: MaybeSend + MaybeSync {
    fn on_resume(&self, _thread: &mluau::Thread) {}
}

pub struct NoopHooks;
impl Hooks for NoopHooks {}

#[derive(Clone)]
/// Task Manager
pub enum TaskManager {
    V2 {
        inner: crate::taskmgr_v2::CoreScheduler,
    },
    V3 {
        inner: crate::taskmgr_v3::CoreSchedulerV3,
    },
}

impl TaskManager {
    /// Creates a new task manager
    pub async fn new(lua: &mluau::Lua, returns: ReturnTracker, hooks: XRc<dyn Hooks>) -> Result<Self, Error> {
        let inner = Self::V2 {
            inner: crate::taskmgr_v2::CoreScheduler::new(lua.weak(), returns, hooks).await?,
        };

        lua.set_app_data(inner.clone());

        Ok(inner)
    }

    /// Creates a new task manager
    pub fn new_v3(lua: &mluau::Lua, returns: ReturnTracker, hooks: XRc<dyn Hooks>) -> Result<Self, Error> {
        let inner = Self::V3 {
            inner: crate::taskmgr_v3::CoreSchedulerV3::new(lua.weak(), returns, hooks),
        };

        lua.set_app_data(inner.clone());

        Ok(inner)
    }

    pub fn returns(&self) -> &ReturnTracker {
        match self {
            TaskManager::V2 { inner } => &inner.returns(),
            TaskManager::V3 { inner } => &inner.returns(),
        }
    }

    fn get_lua(&self) -> Option<mluau::Lua> {
        match self {
            TaskManager::V2 { inner } => inner.get_lua(),
            TaskManager::V3 { inner } => inner.get_lua(),
        }
    }

    /// Adds a waiting thread to the task manager
    #[inline]
    pub fn add_waiting_thread(
        &self,
        thread: mluau::Thread,
        delay_args: Option<mluau::MultiValue>,
        duration: std::time::Duration,
    ) {
        match self {
            TaskManager::V2 { inner } => {
                inner
                .push_event(crate::taskmgr_v2::SchedulerEvent::Wait {
                    delay_args,
                    thread,
                    duration,
                    start_at: std::time::Instant::now(),
                });
            },
            TaskManager::V3 { inner } => {
                if let Some(delay_args) = delay_args {
                    inner.schedule_delay(thread, duration, delay_args);
                } else {
                    inner.schedule_wait(thread, duration);
                }
            },
        }
    }

    /// Adds a deferred thread to the task manager to the front of the queue
    #[inline]
    pub fn add_deferred_thread(&self, thread: mluau::Thread, args: mluau::MultiValue) {
        match self {
            TaskManager::V2 { inner } => {
                inner
                .push_event(crate::taskmgr_v2::SchedulerEvent::DeferredThread { args, thread });
            },
            TaskManager::V3 { inner } => {
                inner.schedule_deferred(thread, args);
            },
        }
    }

    pub fn cancel_thread(&self, thread: &mluau::Thread) -> Result<(), mluau::Error> {
        match self {
            TaskManager::V2 { inner } => {
                inner.cancel_thread(thread)?;
            },
            TaskManager::V3 { inner } => {
                inner.cancel_thread(thread);
            },
        }
        Ok(())
    }

    pub fn add_async<F>(&self, thread: mluau::Thread, fut: F) 
    where 
        F: Future<Output = mluau::Result<mluau::MultiValue>> + MaybeSend + MaybeSync + 'static 
    {
        match self {
            TaskManager::V2 { inner } => {
                inner
                .push_event(crate::taskmgr_v2::SchedulerEvent::AddAsync { thread, fut: Box::pin(fut) });
            },
            TaskManager::V3 { inner } => {
                inner.schedule_async(thread, fut);
            },
        }
    }

    pub async fn wait_till_done(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match self {
            TaskManager::V2 { inner } => {
                inner.wait_till_done().await?;
            },
            TaskManager::V3 { inner } => {
                inner.wait_till_done().await;
            },
        }
        Ok(())
    }

    pub fn stop(&self) {
        match self {
            TaskManager::V2 { inner } => {
                inner.stop();
            },
            TaskManager::V3 { inner } => {
                inner.stop();
            },
        }
    }

    /// Spawns a thread, discarding its output entirely
    pub fn spawn_thread(&self, thread: mluau::Thread, args: mluau::MultiValue) -> Result<(), mluau::Error> {
        thread.resume::<()>(args)?;
        Ok(())
    }

    /// Spawns a thread and then proceeds to get its output properly
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

pub fn get(lua: &'_ mluau::Lua) -> TaskManager {
    lua.app_data_ref::<TaskManager>()
        .expect("Failed to get task manager")
        .clone()
}
