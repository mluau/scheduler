use crate::{XRc, XRefCell};
use std::cell::Cell;
use std::collections::{BinaryHeap, VecDeque};
use std::time::Duration;

pub enum WaitOp {
    // task.wait semantics
    Wait,
    // task.delay semantics
    Delay { args: mlua::MultiValue },
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

pub trait SchedulerFeedback {
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
        _tm: &TaskManager,
        _th: &mlua::Thread,
        _result: mlua::Result<mlua::MultiValue>,
    ) {
        // Do nothing, unless overridden
    }
}

/// Inner task manager state
pub struct TaskManagerInner {
    pub lua: mlua::Lua,
    pub pending_resumes: Cell<usize>,
    pub pending_asyncs: Cell<usize>,
    pub waiting_queue: XRefCell<BinaryHeap<WaitingThread>>,
    pub deferred_queue: XRefCell<VecDeque<DeferredThread>>,
    pub is_running: Cell<bool>,
    pub is_cancelled: Cell<bool>,
    pub feedback: XRc<dyn SchedulerFeedback>,
    pub run_sleep_interval: Duration,
    pub async_task_executor: XRefCell<tokio::task::JoinSet<()>>,
}

#[derive(Clone)]
/// Task Manager
pub struct TaskManager {
    pub inner: XRc<TaskManagerInner>,
}

impl TaskManager {
    /// Creates a new task manager
    pub fn new(
        lua: mlua::Lua,
        feedback: XRc<dyn SchedulerFeedback>,
        run_sleep_interval: Duration,
    ) -> Self {
        Self {
            inner: TaskManagerInner {
                pending_resumes: Cell::new(0),
                pending_asyncs: Cell::new(0),
                waiting_queue: XRefCell::new(BinaryHeap::default()),
                deferred_queue: XRefCell::new(VecDeque::default()),
                is_running: Cell::new(false),
                is_cancelled: Cell::new(false),
                feedback,
                run_sleep_interval,
                lua,
                async_task_executor: XRefCell::new(tokio::task::JoinSet::new()),
            }
            .into(),
        }
    }

    /// Attaches the task manager to the lua state
    pub fn attach(&self) {
        self.inner.lua.set_app_data(self.clone());
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
    pub fn feedback(&self) -> XRc<dyn SchedulerFeedback> {
        self.inner.feedback.clone()
    }

    #[inline(always)]
    /// Resumes a thread to next. yield_now should probably also be used here
    pub fn resume_thread_fast(
        &self,
        thread: &mlua::Thread,
        args: impl mlua::IntoLuaMulti,
    ) -> Option<mlua::Result<mlua::MultiValue>> {
        Some(thread.resume(args))
    }

    /// Adds a waiting thread to the task manager
    pub fn add_waiting_thread(
        &self,
        thread: mlua::Thread,
        op: WaitOp,
        duration: std::time::Duration,
    ) {
        log::debug!("Trying to add thread to waiting queue");
        let mut self_ref = self.inner.waiting_queue.borrow_mut();
        let start = std::time::Instant::now();
        let wake_at = start + duration;
        self_ref.push(WaitingThread {
            thread,
            op,
            start,
            wake_at,
        });
        self.fire_run();
        log::debug!("Added thread to waiting queue");
    }

    /// Removes a waiting thread from the task manager returning the number of threads removed
    pub fn remove_waiting_thread(&self, thread: &mlua::Thread) -> u64 {
        let mut self_ref = self.inner.waiting_queue.borrow_mut();

        let mut removed = 0;
        self_ref.retain(|x| {
            if x.thread != *thread {
                true
            } else {
                removed += 1;
                false
            }
        });

        removed
    }

    /// Adds a deferred thread to the task manager to the front of the queue
    pub fn add_deferred_thread_front(&self, thread: mlua::Thread, args: mlua::MultiValue) {
        let mut self_ref = self.inner.deferred_queue.borrow_mut();
        self_ref.push_front(DeferredThread { thread, args });
        self.fire_run();
    }

    /// Adds a deferred thread to the task manager to the front of the queue
    pub fn add_deferred_thread_back(&self, thread: mlua::Thread, args: mlua::MultiValue) {
        let mut self_ref = self.inner.deferred_queue.borrow_mut();
        self_ref.push_front(DeferredThread { thread, args });
        self.fire_run();
    }

    /// Removes a deferred thread from the task manager returning the number of threads removed
    pub fn remove_deferred_thread(&self, thread: &mlua::Thread) -> u64 {
        let mut self_ref = self.inner.deferred_queue.borrow_mut();

        let mut removed = 0;
        self_ref.retain(|x| {
            if x.thread != *thread {
                true
            } else {
                removed += 1;
                false
            }
        });

        removed
    }

    /// Runs the task manager
    ///
    /// Note that the scheduler will automatically schedule run to be called if needed
    async fn run(&self) {
        if self.is_running() || self.is_cancelled() {
            return; // Quick exit
        }

        self.inner.is_running.set(true);

        loop {
            if self.is_cancelled() {
                break;
            }

            self.process().await;

            tokio::time::sleep(self.inner.run_sleep_interval).await;
        }

        self.inner.is_running.set(false)
    }

    /// To avoid constantly running the task manager, we schedule fires that will run the task manager
    fn fire_run(&self) {
        if self.is_running() || self.is_cancelled() {
            return;
        }

        let self_ref = self.clone();

        tokio::task::spawn_local(async move {
            self_ref.run().await;
        });
    }

    /// Processes the task manager
    pub async fn process(&self) {
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

                self.process_deferred_thread(entry).await;
            }
        }
    }

    /// Processes a deferred thread. Returns true if the thread is still running and should be readded to the list of deferred tasks
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
                let result = {
                    let r = thread_info.thread.resume(thread_info.args);
                    tokio::task::yield_now().await;
                    r
                };

                self.inner.feedback.on_response(
                    "DeferredThread",
                    self,
                    &thread_info.thread,
                    result,
                );
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

                        let result = {
                            let r = thread_info
                                .thread
                                .resume((current_time - start).as_secs_f64());
                            tokio::task::yield_now().await;
                            r
                        };

                        self.inner.feedback.on_response(
                            "WaitingThread",
                            self,
                            &thread_info.thread,
                            result,
                        );
                    }
                    WaitOp::Delay { args } => {
                        let result = {
                            let r = thread_info.thread.resume(args);
                            tokio::task::yield_now().await;
                            r
                        };

                        self.inner.feedback.on_response(
                            "DelayedThread",
                            self,
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
        self.inner.waiting_queue.borrow_mut().clear();
        self.inner.deferred_queue.borrow_mut().clear();
    }

    /// Returns the waiting queue length
    pub fn waiting_len(&self) -> usize {
        self.inner.waiting_queue.borrow().len()
    }

    /// Returns the deferred queue length
    pub fn deferred_len(&self) -> usize {
        self.inner.deferred_queue.borrow().len()
    }

    /// Returns the pending resumes length
    pub fn pending_resumes_len(&self) -> usize {
        self.inner.pending_resumes.get()
    }

    /// Returns the pending asyncs length
    pub fn pending_asyncs_len(&self) -> usize {
        self.inner.pending_asyncs.get()
    }

    /// Returns the number of items in the whole queue
    pub fn len(&self) -> usize {
        self.waiting_len()
            + self.deferred_len()
            + self.pending_resumes_len()
            + self.pending_asyncs_len()
    }

    /// Returns if the queue is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Waits until the task manager is done
    pub async fn wait_till_done(&self, sleep_interval: Duration) {
        while !self.is_cancelled() {
            if self.is_empty() {
                break;
            }

            tokio::time::sleep(sleep_interval).await;
        }
    }
}

pub fn get(lua: &mlua::Lua) -> TaskManager {
    lua.app_data_ref::<TaskManager>()
        .expect("Failed to get task manager")
        .clone()
}
