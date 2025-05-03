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
    pub fn attach(&self) -> Result<(), mlua::Error> { 
        let Some(lua) = self.task_manager.inner.lua.try_upgrade() else {
            return Err(mlua::Error::external("Lua state is not valid"));
        };
        lua.set_app_data(self.clone());
        self.task_manager.attach()?; // Attach the task manager on the scheduler to the lua state
        Ok(())
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
        let resp = thread.resume(args);

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
    ) -> Result<Option<mlua::Result<mlua::MultiValue>>, mlua::Error> {
        let mut rx = {
            let Some(lua) = self.task_manager.inner.lua.try_upgrade() else {
                return Err(mlua::Error::external("Lua state is not valid"));
            };
    
            let ter = lua
                .app_data_ref::<feedbacks::ThreadTracker>()
                .ok_or(mlua::Error::external(
                    "ThreadTracker not attached to Lua state",
                ))?;

            ter.track_thread(&thread)
        }; // Lua is dropped here

        let result = thread.resume(args);

        self.task_manager
            .inner
            .feedback
            .on_response(label, &self.task_manager, &thread, result);

        let mut value: Option<mlua::Result<mlua::MultiValue>> = None;

        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            tokio::select! {
                Some(next) = rx.recv() => {
                    if self.task_manager.inner.lua.try_upgrade().is_none() {
                        log::trace!("Scheduler is no longer valid, exiting...");
                        break;
                    }

                    log::trace!("Received value: {:?}", next);
                    value = Some(next);

                    let status = thread.status();
                    if (status == mlua::ThreadStatus::Finished || status == mlua::ThreadStatus::Error)
                        && rx.is_empty()
                    {
                        log::trace!("Status: {:?}", status);
                        break;
                    }
                }
                _ = ticker.tick() => {
                    if self.task_manager.inner.lua.try_upgrade().is_none() {
                        log::trace!("Scheduler is no longer valid, exiting...");
                        break;
                    }

                    if let Ok(next) = rx.try_recv() {
                        log::trace!("Received value: {:?}", next);
                        value = Some(next);
                    }

                    let status = thread.status();
                    if (status == mlua::ThreadStatus::Finished || status == mlua::ThreadStatus::Error)
                        && rx.is_empty()
                    {
                        log::warn!("Alternative pathway triggered. This is a bug.");
                        break;
                    }
                }
            }
        }

        Ok(value)
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
