use crate::taskmgr_v3::ThreadData;
use crate::{MaybeSend, MaybeSync, XRefCell};
use crate::XRc;
use std::future::Future;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender, UnboundedReceiver};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// A tracker for thread return values
pub(crate) struct ReturnTracker {
    return_tracker: XRefCell<Option<UnboundedSender<mluau::Result<mluau::MultiValue>>>>
}

impl ReturnTracker {
    pub fn new() -> Self {
        Self {
            return_tracker: XRefCell::new(None)
        }
    }

    pub fn is_tracking(&self) -> bool {
        self.return_tracker.borrow().is_some()
    }

    pub fn push_result(&self, result: mluau::Result<mluau::MultiValue>) {
        if let Some(tx) = self.return_tracker.borrow().as_ref() {
            let _ = tx.send(result);
        }
    }

    pub fn track(&self) -> UnboundedReceiver<mluau::Result<mluau::MultiValue>> {
        let (tx, rx) = unbounded_channel();
        let mut tracker = self.return_tracker.borrow_mut();
        *tracker = Some(tx);
        rx
    }

    pub fn stop_track(&self) {
        let mut tracker = self.return_tracker.borrow_mut();
        *tracker = None;
    }
}


pub trait Hooks: MaybeSend + MaybeSync {
    fn on_resume(&self, _thread: &mluau::Thread) {}
}

pub struct NoopHooks;
impl Hooks for NoopHooks {}

#[derive(Clone)]
/// Task Manager
pub enum TaskManager {
    V3 {
        inner: crate::taskmgr_v3::CoreSchedulerV3,
    },
}

impl TaskManager {
    /// Creates a new task manager
    pub fn new(lua: &mluau::Lua, hooks: XRc<dyn Hooks>) -> Result<Self, Error> {
        let inner = Self::V3 {
            inner: crate::taskmgr_v3::CoreSchedulerV3::new(lua.weak(), hooks),
        };

        lua.set_app_data(inner.clone());

        Ok(inner)
    }

    fn get_lua(&self) -> Option<mluau::Lua> {
        match self {
            TaskManager::V3 { inner } => inner.get_lua(),
        }
    }

    /// Tracks a thread's return values
    pub(crate) fn track_thread(
        &self,
        th: &mluau::Thread,
    ) -> UnboundedReceiver<mluau::Result<mluau::MultiValue>> {
        match self {
            TaskManager::V3 { .. } => {
                ThreadData::get(th, |data| data.return_tracker.track())
            },
        }
    }

    /// Pushes a result to a tracked thread
    pub(crate) fn push_result(&self, th: &mluau::Thread, result: mluau::Result<mluau::MultiValue>) {
        match self {
            TaskManager::V3 { .. } => {
                let Some(data) = ThreadData::get_existing(th) else {
                    return;
                };
                data.return_tracker.push_result(result); 
            }
        }
    }

    /// Stops tracking a thread's return values
    pub(crate) fn stop_tracking_thread(&self, th: &mluau::Thread) {
        match self {
            TaskManager::V3 { .. } => {
                let Some(data) = ThreadData::get_existing(th) else {
                    return;
                };
                data.return_tracker.stop_track();
            },
        }
    }

    /// Returns if a thread is being tracked
    pub(crate) fn is_tracking(&self, th: &mluau::Thread) -> bool {
        match self {
            TaskManager::V3 { .. } => {
                let Some(data) = ThreadData::get_existing(th) else {
                    return false;
                };
                data.return_tracker.is_tracking()
            },
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
            TaskManager::V3 { inner } => {
                inner.schedule_deferred(thread, args);
            },
        }
    }

    pub fn cancel_thread(&self, thread: &mluau::Thread) -> Result<(), mluau::Error> {
        match self {
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
            TaskManager::V3 { inner } => {
                inner.schedule_async(thread, fut);
            },
        }
    }

    pub async fn wait_till_done(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        match self {
            TaskManager::V3 { inner } => {
                inner.wait_till_done().await;
            },
        }
        Ok(())
    }

    pub fn stop(&self) {
        match self {
            TaskManager::V3 { inner } => {
                inner.stop();
            },
        }
    }

    /// Spawns a thread and then proceeds to get its output properly
    pub async fn spawn_thread_and_wait(
        &self,
        thread: mluau::Thread,
        args: mluau::MultiValue,
    ) -> mluau::Result<mluau::MultiValue> {
        let mut rx = self.track_thread(&thread);

        let mut value = thread.resume(args);

        loop {
            if self.get_lua().is_none() {
                log::trace!("Scheduler is no longer valid, exiting...");
                break;
            }

            let status = thread.status();
            if (status == mluau::ThreadStatus::Finished || status == mluau::ThreadStatus::Error)
                && rx.is_empty()
            {
                log::trace!("Status: {status:?}");
                break;
            }

            // Wait for the next result
            let Some(next) = rx.recv().await else {
                break;
            };
            log::trace!("Received value: {next:?}");

            value = next;
        }

        self.stop_tracking_thread(&thread);
        value
    }
}

pub fn get(lua: &'_ mluau::Lua) -> TaskManager {
    lua.app_data_ref::<TaskManager>()
        .expect("Failed to get task manager")
        .clone()
}
