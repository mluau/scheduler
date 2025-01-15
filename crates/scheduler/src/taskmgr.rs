use crate::{XRc, XRefCell};
use mlua::IntoLua;
use std::collections::{BinaryHeap, VecDeque};
use std::time::Duration;

#[cfg(not(feature = "fast"))]
use futures_util::StreamExt;

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

#[cfg(not(feature = "send"))]
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
        _result: Option<mlua::Result<mlua::MultiValue>>,
    ) {
        // Do nothing, unless overridden
    }
}

#[cfg(feature = "send")]
pub trait SchedulerFeedback: Send + Sync {
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
        _result: Option<mlua::Result<mlua::MultiValue>>,
    ) {
        // Do nothing, unless overridden
    }
}

/// Inner task manager state
pub struct TaskManagerInner {
    pub lua: mlua::Lua,
    pub pending_resumes: XRefCell<usize>,
    pub pending_asyncs: XRefCell<usize>,
    pub waiting_queue: XRefCell<BinaryHeap<WaitingThread>>,
    pub deferred_queue: XRefCell<VecDeque<DeferredThread>>,
    pub is_running: XRefCell<bool>,
    pub feedback: XRc<dyn SchedulerFeedback>,
    pub run_sleep_interval: Duration,
    pub scheduled_fires: XRefCell<BinaryHeap<std::time::Instant>>,
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
                pending_resumes: XRefCell::new(0),
                pending_asyncs: XRefCell::new(0),
                waiting_queue: XRefCell::new(BinaryHeap::default()),
                deferred_queue: XRefCell::new(VecDeque::default()),
                is_running: XRefCell::new(false),
                feedback,
                run_sleep_interval,
                lua,
                scheduled_fires: XRefCell::new(BinaryHeap::default()),
                async_task_executor: XRefCell::new(tokio::task::JoinSet::new()),
            }
            .into(),
        }
    }

    /// Attaches the task manager to the lua state
    pub fn attach(&self) {
        self.inner.lua.set_app_data(self.clone());

        // Also save error userdata
        let error_userdata = ErrorUserdata {}.into_lua(&self.inner.lua).unwrap();
        self.inner
            .lua
            .set_app_data(ErrorUserdataValue(error_userdata));
    }

    /// Returns whether the task manager is running
    pub fn is_running(&self) -> bool {
        *(self.inner.is_running.borrow())
    }

    /// Returns the feedback stored in the task manager
    pub fn feedback(&self) -> XRc<dyn SchedulerFeedback> {
        self.inner.feedback.clone()
    }

    #[cfg(feature = "fast")]
    /// Resumes a thread to next. yield_now should probably also be used here
    pub fn resume_thread_fast(
        &self,
        thread: &mlua::Thread,
        args: mlua::MultiValue,
    ) -> Option<mlua::Result<mlua::MultiValue>> {
        if thread.status() != mlua::ThreadStatus::Resumable {
            return None;
        }

        Some(thread.resume(args))
    }

    #[cfg(not(feature = "fast"))]
    /// Resumes a thread to next
    pub async fn resume_thread_slow(
        &self,
        thread: mlua::Thread,
        args: mlua::MultiValue,
    ) -> Option<mlua::Result<mlua::MultiValue>> {
        #[cfg(feature = "send")]
        self.inner.pending_resumes.fetch_add(1, Ordering::AcqRel);
        #[cfg(not(feature = "send"))]
        self.inner.pending_resumes.fetch_add(1, Ordering::Relaxed);

        let mut async_thread = match thread.into_async(args) {
            Ok(async_thread) => async_thread,
            Err(e) => {
                #[cfg(feature = "send")]
                self.inner.pending_resumes.fetch_sub(1, Ordering::AcqRel);
                #[cfg(not(feature = "send"))]
                self.inner.pending_resumes.fetch_sub(1, Ordering::Relaxed);

                return Some(Err(e));
            }
        };

        let next = async_thread.next().await;

        #[cfg(feature = "send")]
        self.inner.pending_resumes.fetch_sub(1, Ordering::AcqRel);
        #[cfg(not(feature = "send"))]
        self.inner.pending_resumes.fetch_sub(1, Ordering::Relaxed);

        next
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
            start: std::time::Instant::now(),
            wake_at,
        });
        self.fire_run(duration);
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
        self.fire_run(self.inner.run_sleep_interval);
    }

    /// Adds a deferred thread to the task manager to the front of the queue
    pub fn add_deferred_thread_back(&self, thread: mlua::Thread, args: mlua::MultiValue) {
        let mut self_ref = self.inner.deferred_queue.borrow_mut();
        self_ref.push_front(DeferredThread { thread, args });
        self.fire_run(self.inner.run_sleep_interval);
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
        if self.is_running() {
            return; // Quick exit
        }

        self.inner.scheduled_fires.borrow_mut().clear();

        const MAX_RUNS: usize = 100;

        *self.inner.is_running.borrow_mut() = true;

        let mut extra_runs = 0;
        loop {
            if self.is_empty() || !self.is_running() {
                extra_runs += 1;
                if extra_runs > MAX_RUNS {
                    break;
                }
            } else {
                //log::debug!("Processing task manager");
                self.process().await;
            }

            tokio::time::sleep(self.inner.run_sleep_interval).await;
        }

        *self.inner.is_running.borrow_mut() = false;
    }

    /// To avoid constantly running the task manager, we schedule fires that will run the task manager
    fn fire_run(&self, after: Duration) {
        if self.is_running() {
            return;
        }

        let mut scheduled_fires = self.inner.scheduled_fires.borrow_mut();

        let schedule_instant = std::time::Instant::now() + after;
        if let Some(scheduled_fires) = scheduled_fires.peek() {
            if schedule_instant < *scheduled_fires {
                return; // We already have a scheduled fire that is sooner
            }
        }

        scheduled_fires.push(schedule_instant);

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
                #[cfg(not(feature = "fast"))]
                let result = self
                    .resume_thread_fast(thread_info.thread.clone(), thread_info.args)
                    .await;
                #[cfg(feature = "fast")]
                let result = {
                    let r = self.resume_thread_fast(&thread_info.thread, thread_info.args);
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

                        let args = mlua::MultiValue::from_iter([mlua::Value::Number(
                            (current_time - start).as_secs_f64(),
                        )]);

                        #[cfg(not(feature = "fast"))]
                        let result = self
                            .resume_thread_slow(thread_info.thread.clone(), args)
                            .await;
                        #[cfg(feature = "fast")]
                        let result = {
                            let r = self.resume_thread_fast(&thread_info.thread, args);
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
                        #[cfg(not(feature = "fast"))]
                        let result = self
                            .resume_thread_fast(thread_info.thread.clone(), args)
                            .await;
                        #[cfg(feature = "fast")]
                        let result = {
                            let r = self.resume_thread_fast(&thread_info.thread, args);
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
        *self.inner.is_running.borrow_mut() = false;
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
        *self.inner.pending_resumes.borrow()
    }

    /// Returns the pending asyncs length
    pub fn pending_asyncs_len(&self) -> usize {
        *self.inner.pending_asyncs.borrow()
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
        while self.is_running() {
            if self.is_empty() {
                break;
            }

            tokio::time::sleep(sleep_interval).await;
        }
    }
}

#[derive(Clone)]
pub struct ErrorUserdata {}

impl mlua::UserData for ErrorUserdata {}

pub struct ErrorUserdataValue(pub mlua::Value);

pub fn get(lua: &mlua::Lua) -> TaskManager {
    lua.app_data_ref::<TaskManager>()
        .expect("Failed to get task manager")
        .clone()
}
