pub mod feedbacks;
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
    pub fn attach(&self) {
        self.task_manager.inner.lua.set_app_data(self.clone());
        self.task_manager.attach(); // Attach the task manager on the scheduler to the lua state
    }

    /// Returns the scheduler given a lua
    pub fn get(lua: &mlua::Lua) -> Self {
        lua.app_data_ref::<Self>()
            .expect("No scheduler attached")
            .clone()
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

    /// Spawns a thread, discarding its output entirely
    pub async fn spawn_thread(&self, label: &str, thread: mlua::Thread, args: mlua::MultiValue) {
        let resp = self
            .task_manager
            .resume_thread(label, thread.clone(), args)
            .await;

        self.task_manager
            .inner
            .feedback
            .on_response(label, &self.task_manager, &thread, resp);
    }

    /// Spawns a thread and then proceeds to get its output properly
    ///
    /// This requires ThreadTracker to be attached to the scheduler
    pub async fn spawn_thread_and_wait(
        &self,
        label: &str,
        thread: mlua::Thread,
        args: mlua::MultiValue,
    ) -> Option<mlua::Result<mlua::MultiValue>> {
        let ter = self
            .task_manager
            .inner
            .lua
            .app_data_ref::<feedbacks::ThreadTracker>()
            .expect("No ThreadResultTracker attached");

        let mut rx = ter.track_thread(&thread);

        let result = self
            .task_manager
            .resume_thread(label, thread.clone(), args)
            .await;

        self.task_manager
            .inner
            .feedback
            .on_response(label, &self.task_manager, &thread, result);

        let mut value: Option<mlua::Result<mlua::MultiValue>> = None;

        while let Some(next) = rx.recv().await {
            log::debug!("Received value: {:?}", next);
            value = next;

            let status = thread.status();
            if (status == mlua::ThreadStatus::Finished || status == mlua::ThreadStatus::Error)
                && rx.is_empty()
            {
                break;
            }
        }

        value
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
