use crate::{XRc, XRefCell, XBool, XUsize};
use std::cell::Cell;
use std::collections::{BinaryHeap, VecDeque};
use std::time::Duration;

pub enum WaitOp {
    // task.wait semantics
    Wait,
    // task.delay semantics
    Delay { 
        args: mlua::MultiValue,
    },
}

pub struct WaitingThread {
    thread: mlua::Thread,
    op: WaitOp,
    start: std::time::Instant,
    wake_at: std::time::Instant,
}

impl std::cmp::PartialEq for WaitingThread {
    fn eq(&self, other: &Self) -> bool {
        self.wake_at == other.wake_at
    }
}

impl std::cmp::Eq for WaitingThread {}

impl std::cmp::PartialOrd for WaitingThread {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::cmp::Ord for WaitingThread {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse order
        (other.wake_at).cmp(&(self.wake_at))
    }
}

pub struct DeferredThread {
    thread: mlua::Thread,
    args: mlua::MultiValue,
}

pub trait SchedulerFeedback: crate::MaybeSend + crate::MaybeSync {
    /// Function that is called whenever a thread is added/known to the task manager
    ///
    /// Contains both the creator thread and the thread that was added
    fn on_thread_add(
        &self,
        _label: &str,
        _creator: &mlua::Thread,
        _thread: &mlua::Thread,
    ) -> mlua::Result<()> {
        // Do nothing, unless overridden
        Ok(())
    }

    /// Function that is called when any response, Ok or Error occurs
    fn on_response(
        &self,
        _label: &str,
        _th: &mlua::Thread,
        _result: mlua::Result<mlua::MultiValue>,
    ) {
        // Do nothing, unless overridden
    }
}

/// Inner task manager state
pub struct TaskManagerInner {
    pub lua: mlua::WeakLua,
    pub pending_asyncs: XUsize,
    pub waiting_queue: XRefCell<BinaryHeap<WaitingThread>>,
    pub deferred_queue: XRefCell<VecDeque<DeferredThread>>,
    pub is_running: XBool,
    pub is_cancelled: XBool,
    pub feedback: XRc<dyn SchedulerFeedback>,
    pub async_task_executor: XRefCell<tokio::task::JoinSet<()>>,
}

#[derive(Clone)]
/// Task Manager
pub struct TaskManager {
    #[cfg(feature = "v2_taskmgr")]
    pub(crate) inner: XRc<crate::taskmgr_v2::CoreScheduler>,
    #[cfg(not(feature = "v2_taskmgr"))]
    pub(crate) inner: XRc<TaskManagerInner>,
}

impl TaskManager {
    /// Creates a new task manager
    pub fn new(
        lua: &mlua::Lua,
        feedback: XRc<dyn SchedulerFeedback>,
    ) -> Self {
        #[cfg(feature = "v2_taskmgr")]
        {
            Self {
                inner: crate::taskmgr_v2::CoreScheduler::new(lua.weak(), feedback).into()
            }
        }
        #[cfg(not(feature = "v2_taskmgr"))]
        {
            Self {
                inner: TaskManagerInner {
                    pending_asyncs: XUsize::new(0),
                    waiting_queue: XRefCell::new(BinaryHeap::default()),
                    deferred_queue: XRefCell::new(VecDeque::default()),
                    is_running: XBool::new(false),
                    is_cancelled: XBool::new(false),
                    feedback,
                    lua: lua.weak(),
                    async_task_executor: XRefCell::new(tokio::task::JoinSet::new()),
                }
                .into(),
            }
        }
    }

    /// Checks if the lua state is valid
    fn check_lua(&self) -> bool {
        self.inner.lua.try_upgrade().is_some()
    }

    /// Tries to get strong ref to lua
    pub fn get_lua(&self) -> Option<mlua::Lua> {
        self.inner.lua.try_upgrade()
    }

    /// Attaches the task manager to the lua state. Note that run_in_task (etc.) must also be called
    pub fn attach(&self) -> Result<(), mlua::Error> {
        let Some(lua) = self.get_lua() else {
            return Err(mlua::Error::RuntimeError("Failed to upgrade lua".to_string()));
        };
        lua.set_app_data(self.clone());
        Ok(())
    }

    /// Returns whether the task manager has been cancelled
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled.get()
    }

    /// Returns whether the task manager is running
    pub fn is_running(&self) -> bool {
        self.inner.is_running.get()
    }

    /// Returns the feedback stored in the task manager
    pub fn feedback(&self) -> &dyn SchedulerFeedback {
        &*self.inner.feedback
    }

    pub fn incr_async(&self) {
        let current_pending = self.inner.pending_asyncs.get();
        self.inner.pending_asyncs.set(current_pending + 1);
    }

    pub fn decr_async(&self) {
        let current_pending = self.inner.pending_asyncs.get();
        self.inner.pending_asyncs.set(current_pending - 1);
    }

    /// Adds a waiting thread to the task manager
    pub fn add_waiting_thread(
        &self,
        thread: mlua::Thread,
        delay_args: Option<mlua::MultiValue>,
        duration: std::time::Duration,
    ) {
        #[cfg(feature = "v2_taskmgr")]
        {
            self.inner.push_event(crate::taskmgr_v2::SchedulerEvent::Wait {
                delay_args,
                thread,
                duration,
                start_at: std::time::Instant::now()
            });
        }
        #[cfg(not(feature = "v2_taskmgr"))]
        {
            let op = match delay_args {
                Some(delay_args) => WaitOp::Delay { args: delay_args },
                None => WaitOp::Wait
            };

            log::trace!("Trying to add thread to waiting queue");
            let mut self_ref = self.inner.waiting_queue.borrow_mut();
            let start = std::time::Instant::now();
            let wake_at = start + duration;
            self_ref.push(WaitingThread {
                thread,
                op,
                start,
                wake_at,
            });
            log::trace!("Added thread to waiting queue");
        }
    }

    /// Removes a waiting thread from the task manager
    pub fn remove_waiting_thread(&self, thread: &mlua::Thread) {
        #[cfg(feature = "v2_taskmgr")]
        {
            self.inner.push_event(crate::taskmgr_v2::SchedulerEvent::CancelWaitThread {
                xid: crate::XId::from_ptr(thread.to_pointer())
            });
        }
        #[cfg(not(feature = "v2_taskmgr"))]
        {
            let mut self_ref = self.inner.waiting_queue.borrow_mut();

            self_ref.retain(|x| {
                if x.thread != *thread {
                    true
                } else {
                    false
                }
            });
        }
    }

    /// Adds a deferred thread to the task manager to the front of the queue
    pub fn add_deferred_thread(&self, thread: mlua::Thread, args: mlua::MultiValue) {
        #[cfg(feature = "v2_taskmgr")]
        {
            self.inner.push_event(crate::taskmgr_v2::SchedulerEvent::DeferredThread {
                args,
                thread,
            });
        }
        #[cfg(not(feature = "v2_taskmgr"))]
        {
            let mut self_ref = self.inner.deferred_queue.borrow_mut();
            self_ref.push_back(DeferredThread { thread, args });
        }
    }

    /// Removes a deferred thread from the task manager
    pub fn remove_deferred_thread(&self, thread: &mlua::Thread) {
        #[cfg(feature = "v2_taskmgr")]
        {
            self.inner.push_event(crate::taskmgr_v2::SchedulerEvent::CancelDeferredThread {
                xid: crate::XId::from_ptr(thread.to_pointer())
            });
        }
        #[cfg(not(feature = "v2_taskmgr"))]
        {
            let mut self_ref = self.inner.deferred_queue.borrow_mut();

            self_ref.retain(|x| {
                if x.thread != *thread {
                    true
                } else {
                    false
                }
            });
        }
    }

    /// Runs the task manager
    ///
    /// Note that the scheduler will automatically schedule run to be called if needed
    async fn run(&self, mut ticker: tokio::sync::broadcast::Receiver<()>) {
        #[cfg(feature = "v2_taskmgr")]
        {
            self.inner.run().await
        }
        #[cfg(not(feature = "v2_taskmgr"))]
        {
            if self.is_running() || self.is_cancelled() || !self.check_lua() {
                return; // Quick exit
            }

            self.inner.is_running.set(true);

            log::debug!("Task manager started");

            loop {
                if self.is_cancelled() || !self.check_lua() {
                    break;
                }

                self.process().await;

                let _ = ticker.recv().await;
            }

            self.inner.is_running.set(false)
        }
    }

    /// Helper method to start up the task manager
    /// from a synchronous context
    pub fn run_in_task(&self, mut ticker: tokio::sync::broadcast::Receiver<()>) {
        if self.is_running() || self.is_cancelled() || !self.check_lua() {
            return;
        }

        log::debug!("Firing up task manager");

        let self_ref = self.clone();

        #[cfg(feature = "send")]
        tokio::task::spawn(async move {
            self_ref.run(ticker).await;
        });
        #[cfg(not(feature = "send"))]
        tokio::task::spawn_local(async move {
            self_ref.run(ticker).await;
        });
    }

    /// Processes the task manager
    #[cfg(not(feature = "v2_taskmgr"))]
    async fn process(&self) {
        /*log::debug!(
            "Queue Length: {}, Running: {}",
            self.inner.len(),
            self.inner.is_running()
        );*/

        // Process all threads in the queue

        //log::debug!("Queue Length After Defer: {}", self.inner.len());
        {
            loop {
                let current_time = std::time::Instant::now();

                // Pop element from self_ref
                let entry = {
                    let mut self_ref = self.inner.waiting_queue.borrow_mut();

                    // Peek and check
                    if let Some(entry) = self_ref.peek() {
                        if entry.wake_at > current_time {
                            break;
                        }
                    }

                    // Pop out the peeked element
                    let Some(entry) = self_ref.pop() else {
                        break;
                    };

                    entry
                };

                if !self.check_lua() {
                    break;
                }

                self.process_waiting_thread(entry, current_time).await;
            }
        }

        {
            loop {
                // Pop element from self_ref
                let entry = {
                    let mut self_ref = self.inner.deferred_queue.borrow_mut();

                    let Some(entry) = self_ref.pop_back() else {
                        break;
                    };

                    entry
                };

                if !self.check_lua() {
                    break;
                }

                self.process_deferred_thread(entry).await;
            }
        }
    }

    /// Processes a deferred thread.
    async fn process_deferred_thread(&self, thread_info: DeferredThread) {
        /*
            if coroutine.status(data.thread) ~= "dead" then
               resume_with_error_check(data.thread, table.unpack(data.args))
            end
        */
        match thread_info.thread.status() {
            mlua::ThreadStatus::Error | mlua::ThreadStatus::Finished => {}
            _ => {
                //log::debug!("Trying to resume deferred thread");
                {
                    let r = thread_info.thread.resume(thread_info.args);
                    self.inner.feedback.on_response(
                        "DeferredThread",
                        &thread_info.thread,
                        r,
                    );    
                    tokio::task::yield_now().await;
                };
            }
        }
    }

    /// Processes a waiting thread
    async fn process_waiting_thread(
        &self,
        thread_info: WaitingThread,
        current_time: std::time::Instant,
    ) {
        /*
        if coroutine.status(thread) == "dead" then
        elseif type(data) == "table" and last_tick >= data.resume then
            if data.start then
                resume_with_error_check(thread, last_tick - data.start)
            else
                resume_with_error_check(thread, table.unpack(data, 1, data.n))
            end
        else
            waiting_threads[thread] = data
        end
                 */
        match thread_info.thread.status() {
            mlua::ThreadStatus::Error | mlua::ThreadStatus::Finished => {}
            _ => {
                /*log::debug!(
                    "Resuming waiting thread, start: {:?}, duration: {:?}, current_time: {:?}",
                    start,
                    duration,
                    current_time
                );*/

                // resume_with_error_check(thread, table.unpack(data, 1, data.n))
                match thread_info.op {
                    WaitOp::Wait => {
                        // Push time elapsed
                        let start = thread_info.start;

                        {
                            let r = thread_info
                                .thread
                                .resume((current_time - start).as_secs_f64());
                            self.inner.feedback.on_response(
                                "WaitingThread",
                                &thread_info.thread,
                                r,
                            );        

                            tokio::task::yield_now().await;
                        }
                    }
                    WaitOp::Delay { args } => {
                        let result = {
                            let r = thread_info.thread.resume(args);
                            tokio::task::yield_now().await;
                            r
                        };

                        self.inner.feedback.on_response(
                            "DelayedThread",
                            &thread_info.thread,
                            result,
                        );
                    }
                };
            }
        }
    }

    /// Stops the task manager
    pub fn stop(&self) {
        self.inner.is_cancelled.set(true);
    }

    /// Unstops the task manager
    pub fn unstop(&self) {
        self.inner.is_cancelled.set(false);
    }

    /// Clears the task manager queues completely
    pub fn clear(&self) {
        #[cfg(feature = "v2_taskmgr")]
        {
            self.inner.push_event(crate::taskmgr_v2::SchedulerEvent::Clear {});
        }
        #[cfg(not(feature = "v2_taskmgr"))]
        {
            self.inner.waiting_queue.borrow_mut().clear();
            self.inner.deferred_queue.borrow_mut().clear();
        }
    }

    /// Returns the waiting queue length
    pub fn waiting_len(&self) -> usize {
        #[cfg(feature = "v2_taskmgr")]
        { self.inner.wait_count.get() }
        #[cfg(not(feature = "v2_taskmgr"))]
        self.inner.waiting_queue.borrow().len()
    }

    /// Returns the deferred queue length
    pub fn deferred_len(&self) -> usize {
        #[cfg(feature = "v2_taskmgr")]
        { 0 } // todo
        #[cfg(not(feature = "v2_taskmgr"))]
        self.inner.deferred_queue.borrow().len()
    }

    /// Returns the pending asyncs length
    pub fn pending_asyncs_len(&self) -> usize {
        self.inner.pending_asyncs.get()
    }

    /// Returns the number of items in the whole queue
    pub fn len(&self) -> usize {
        self.waiting_len()
            + self.deferred_len()
            + self.pending_asyncs_len()
    }

    /// Returns if the queue is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Waits until the task manager is done
    pub async fn wait_till_done(&self, sleep_interval: Duration) {
        while !self.is_cancelled() {
            tokio::time::sleep(sleep_interval).await;

            if self.is_empty() {
                break;
            }
        }
    }
}

pub fn get(lua: &mlua::Lua) -> mlua::AppDataRef<TaskManager> {
    lua.app_data_ref::<TaskManager>()
        .expect("Failed to get task manager")
}
