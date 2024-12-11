pub mod traits;

use mlua_scheduler::taskmgr::TaskManager;
use std::ops::Deref;
use std::sync::atomic::AtomicU8;

/// A wrapper around TaskManager to make stuff easier
pub struct Scheduler {
    /// The underlying task manager
    task_manager: TaskManager,

    /// The exit code that has been set prior to exiting
    exit_code: AtomicU8,
}

impl Scheduler {
    pub fn new(task_manager: TaskManager) -> Self {
        Self {
            task_manager,
            exit_code: AtomicU8::new(0),
        }
    }
}

impl Deref for Scheduler {
    type Target = TaskManager;

    fn deref(&self) -> &Self::Target {
        &self.task_manager
    }
}

impl Scheduler {
    /// Returns the currently set exit code
    pub fn exit_code(&self) -> u8 {
        self.exit_code.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Exits the scheduler with the given exit code
    ///
    /// Note: the scheduler is not guaranteed to exit immedately if it is in the middle of processing tasks
    /// but it *will* exit before the next processing cycle
    ///
    ///
    /// There are two options on handling exiting:
    /// - Use ``wait_till_done`` to wait until the scheduler has exited
    /// - Use ``clear`` to clear all tasks from the queue after calling ``exit_with_code``
    ///
    /// Depending on use case, one or both of these may be necessary
    pub fn exit_with_code(&mut self, code: u8) {
        self.exit_code
            .store(code, std::sync::atomic::Ordering::Release);
        self.stop();
    }
}
