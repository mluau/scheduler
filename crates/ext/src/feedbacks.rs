use std::collections::HashMap;

use mlua_scheduler::{taskmgr::SchedulerFeedback, TaskManager, XRc, XRefCell};

#[cfg(not(feature = "multithread"))]
#[derive(Hash, Eq, PartialEq)]
pub struct ThreadPtr(*const std::ffi::c_void);

#[cfg(feature = "multithread")]
#[derive(Hash, Eq, PartialEq)]
pub struct ThreadPtr(String);

#[cfg(not(feature = "multithread"))]
impl ThreadPtr {
    pub fn new(thread: &mlua::Thread) -> Self {
        Self(thread.to_pointer())
    }
}

#[cfg(feature = "multithread")]
impl ThreadPtr {
    pub fn new(thread: &mlua::Thread) -> Self {
        Self(format!("{:?}", thread.to_pointer()))
    }
}

/// Tracks the threads known to the scheduler to the thread which initiated them
#[derive(Clone)]
pub struct ThreadTracker {
    #[allow(clippy::type_complexity)]
    pub returns: XRc<
        XRefCell<
            HashMap<
                ThreadPtr,
                tokio::sync::mpsc::UnboundedSender<Option<mlua::Result<mlua::MultiValue>>>,
            >,
        >,
    >,
}

impl Default for ThreadTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadTracker {
    /// Creates a new thread tracker
    pub fn new() -> Self {
        Self {
            returns: XRc::new(XRefCell::new(HashMap::new())),
        }
    }

    /// Track a threads result
    pub fn track_thread(
        &self,
        th: &mlua::Thread,
    ) -> tokio::sync::mpsc::UnboundedReceiver<Option<mlua::Result<mlua::MultiValue>>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.returns.borrow_mut().insert(ThreadPtr::new(th), tx);

        rx
    }
}

impl SchedulerFeedback for ThreadTracker {
    fn on_thread_add(
        &self,
        _label: &str,
        _creator: &mlua::Thread,
        _thread: &mlua::Thread,
    ) -> mlua::Result<()> {
        Ok(())
    }

    fn on_response(
        &self,
        _label: &str,
        _tm: &TaskManager,
        th: &mlua::Thread,
        result: Option<Result<mlua::MultiValue, mlua::Error>>,
    ) {
        if let Some(tx) = self.returns.borrow_mut().get(&ThreadPtr::new(th)) {
            let _ = tx.send(match result {
                Some(Ok(mv)) => Some(Ok(mv)),
                Some(Err(e)) => Some(Err(e)),
                None => None,
            });
        }
    }
}
