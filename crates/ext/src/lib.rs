pub mod traits;

use mlua_scheduler::taskmgr::TaskManager;
use mlua_scheduler::XRc;
use std::ops::Deref;
use std::sync::atomic::AtomicU8;

#[derive(Clone)]
/// A wrapper around TaskManager to make stuff easier
pub struct Scheduler {
    /// The underlying task manager
    task_manager: TaskManager,

    /// The exit code that has been set prior to exiting
    exit_code: XRc<AtomicU8>,
}

impl Scheduler {
    pub fn new(task_manager: TaskManager) -> Self {
        Self {
            task_manager,
            exit_code: AtomicU8::new(0).into(),
        }
    }

    /// Attaches the scheduler to the Lua state
    ///
    /// Note: this method also attaches the inner task manager itself
    pub fn attach(&self, lua: &mlua::Lua) {
        lua.set_app_data(self.clone());
        self.task_manager.attach(lua); // Attach the task manager on the scheduler to the lua state
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
    pub fn exit_with_code(&self, code: u8) {
        self.exit_code
            .store(code, std::sync::atomic::Ordering::Release);
        self.stop();
    }

    /// Exits the scheduler with the given exit code and clears the queue to force an immediate exit
    pub fn exit_with_code_clear(&self, code: u8) {
        self.exit_with_code(code);
        self.clear();
    }
}
