use crate::{XRc, XRefCell};
use futures_util::StreamExt;
use mlua::IntoLua;
use std::collections::{BinaryHeap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::Poll;
use std::time::Duration;

pub struct WaitingThread {
    thread: ThreadInfo,
    start: std::time::Instant,
    duration: std::time::Duration,
}

impl std::cmp::PartialEq for WaitingThread {
    fn eq(&self, other: &Self) -> bool {
        self.start + self.duration == other.start + other.duration
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
        (other.start + other.duration).cmp(&(self.start + self.duration))
    }
}

pub struct DeferredThread {
    thread: ThreadInfo,
}

#[derive(Debug)]
pub struct ThreadInfo {
    pub thread: mlua::Thread,
    pub args: mlua::MultiValue,
}

pub struct AsyncThreadInfo {
    pub thread: mlua::Thread,
    #[cfg(feature = "send")]
    pub callback: Pin<Box<dyn (Future<Output = mlua::Result<mlua::MultiValue>>) + Send + Sync>>,
    #[cfg(not(feature = "send"))]
    pub callback: Pin<Box<dyn (Future<Output = mlua::Result<mlua::MultiValue>>)>>,
}

#[cfg(not(feature = "send"))]
pub trait SchedulerFeedback {
    /// Function that is called whenever a thread is added/known to the task manager
    ///
    /// Contains both the creator thread and the thread that was added
    fn on_thread_add(
        &self,
        label: &str,
        creator: &mlua::Thread,
        thread: &mlua::Thread,
    ) -> mlua::Result<()>;

    /// Function that is called when any response, Ok or Error occurs
    fn on_response(
        &self,
        label: &str,
        tm: &TaskManager,
        th: &mlua::Thread,
        result: Option<&mlua::Result<mlua::MultiValue>>,
    );
}

#[cfg(feature = "send")]
pub trait SchedulerFeedback: Send + Sync {
    /// Function that is called whenever a thread is added/known to the task manager
    ///
    /// Contains both the creator thread and the thread that was added
    fn on_thread_add(
        &self,
        label: &str,
        creator: &mlua::Thread,
        thread: &mlua::Thread,
    ) -> mlua::Result<()>;

    /// Function that is called when any response, Ok or Error occurs
    fn on_response(
        &self,
        label: &str,
        tm: &TaskManager,
        th: &mlua::Thread,
        result: Option<&mlua::Result<mlua::MultiValue>>,
    );
}

/// Inner task manager state
pub struct TaskManagerInner {
    pub lua: mlua::Lua,
    pub pending_threads_count: XRc<AtomicU64>,
    pub async_queue: XRefCell<VecDeque<AsyncThreadInfo>>,
    pub waiting_queue: XRefCell<BinaryHeap<WaitingThread>>,
    pub deferred_queue: XRefCell<VecDeque<DeferredThread>>,
    pub is_running: AtomicBool,
    pub feedback: XRc<dyn SchedulerFeedback>,
}

#[derive(Clone)]
/// Task Manager
pub struct TaskManager {
    pub inner: XRc<TaskManagerInner>,
}

impl TaskManager {
    /// Creates a new task manager
    pub fn new(lua: mlua::Lua, feedback: XRc<dyn SchedulerFeedback>) -> Self {
        Self {
            inner: TaskManagerInner {
                pending_threads_count: XRc::new(AtomicU64::new(0)),
                async_queue: XRefCell::new(VecDeque::default()),
                waiting_queue: XRefCell::new(BinaryHeap::default()),
                deferred_queue: XRefCell::new(VecDeque::default()),
                is_running: AtomicBool::new(false),
                feedback,
                lua,
            }
            .into(),
        }
    }

    /// Attaches the task manager to the lua state
    pub fn attach(&self, lua: &mlua::Lua) {
        lua.set_app_data(self.clone());

        // Also save error userdata
        let error_userdata = ErrorUserdata {}.into_lua(lua).unwrap();
        lua.set_app_data(ErrorUserdataValue(error_userdata));
    }

    /// Returns whether the task manager is running
    pub fn is_running(&self) -> bool {
        self.inner.is_running.load(Ordering::Acquire)
    }

    /// Resumes a thread to next
    pub async fn resume_thread<'a>(
        &self,
        label: &str,
        thread: mlua::Thread,
        args: mlua::MultiValue,
    ) -> Option<mlua::Result<mlua::MultiValue>> {
        log::debug!("StartResumeThread: {}", label);

        self.inner
            .pending_threads_count
            .fetch_add(1, Ordering::Relaxed);

        let mut async_thread = thread.into_async::<mlua::MultiValue>(args);

        let next = async_thread.next().await;

        self.inner
            .pending_threads_count
            .fetch_sub(1, Ordering::Relaxed);

        log::debug!("EndResumeThread {}", label);

        next
    }

    /// Adds an async thread to the task manager
    pub fn add_async_thread(
        &self,
        thread: mlua::Thread,
        args: mlua::MultiValue,
        callback: crate::r#async::AsyncCallbackData,
    ) {
        let mut callback = callback.callback;

        let lua = self.inner.lua.clone();
        let args_ref = args.clone();

        let fut = async move { callback.call(lua, args_ref).await };
        log::debug!("Trying to add async thread to queue");
        let mut self_ref = self.inner.async_queue.borrow_mut();
        self_ref.push_front(AsyncThreadInfo {
            thread,
            callback: Box::pin(fut),
        });
        log::debug!("Added async thread to queue");
    }

    /// Adds a waiting thread to the task manager
    pub fn add_waiting_thread(
        &self,
        thread: mlua::Thread,
        args: mlua::MultiValue,
        duration: std::time::Duration,
    ) {
        log::debug!("Trying to add thread to waiting queue");
        let tinfo = ThreadInfo { thread, args };
        let mut self_ref = self.inner.waiting_queue.borrow_mut();
        self_ref.push(WaitingThread {
            thread: tinfo,
            start: std::time::Instant::now(),
            duration,
        });
        log::debug!("Added thread to waiting queue");
    }

    /// Adds a deferred thread to the task manager to the front of the queue
    pub fn add_deferred_thread_front(&self, thread: mlua::Thread, args: mlua::MultiValue) {
        log::debug!("Adding deferred thread to queue");
        let tinfo = ThreadInfo { thread, args };
        let mut self_ref = self.inner.deferred_queue.borrow_mut();
        self_ref.push_front(DeferredThread { thread: tinfo });
        log::debug!("Added deferred thread to queue");
    }

    /// Adds a deferred thread to the task manager to the front of the queue
    pub fn add_deferred_thread_back(&self, thread: mlua::Thread, args: mlua::MultiValue) {
        log::debug!("Adding deferred thread to queue");
        let tinfo = ThreadInfo { thread, args };
        let mut self_ref = self.inner.deferred_queue.borrow_mut();
        self_ref.push_back(DeferredThread { thread: tinfo });
        log::debug!("Added deferred thread to queue");
    }

    /// Runs the task manager
    pub async fn run(&self, sleep_interval: Duration) -> Result<(), mlua::Error> {
        self.inner.is_running.store(true, Ordering::Relaxed);

        //log::debug!("Task Manager started");

        loop {
            if !self.is_running() {
                break;
            }

            //log::debug!("Processing task manager");
            self.process().await?;

            tokio::time::sleep(sleep_interval).await;
        }

        Ok(())
    }

    /// Processes the task manager
    pub async fn process(&self) -> Result<(), mlua::Error> {
        /*log::debug!(
            "Queue Length: {}, Running: {}",
            self.inner.len(),
            self.inner.is_running()
        );*/

        // Process all threads in the queue

        let mut readd_async_list = Vec::new();
        {
            loop {
                // Pop element from self_ref
                let entry = {
                    let mut self_ref = self.inner.async_queue.borrow_mut();

                    let Some(entry) = self_ref.pop_back() else {
                        break;
                    };

                    entry
                };

                if let Some(entry) = self.process_async_thread(entry).await? {
                    readd_async_list.push(entry);
                }
            }
        }

        //log::debug!("Queue Length After Defer: {}", self.inner.len());
        {
            loop {
                // Pop element from self_ref
                let entry = {
                    let mut self_ref = self.inner.waiting_queue.borrow_mut();

                    let Some(entry) = self_ref.pop() else {
                        break;
                    };

                    entry
                };

                if let Some(entry) = self.process_waiting_thread(entry).await {
                    let mut self_ref = self.inner.waiting_queue.borrow_mut();
                    self_ref.push(entry);
                    break; // Because we have a binary heap, we can break here as order is maintained
                }
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

        {
            let mut self_ref = self.inner.async_queue.borrow_mut();
            for entry in readd_async_list {
                self_ref.push_back(entry);
            }
        }

        // Check all_threads, removing all finished threads and resuming all threads not in deferred_queue or waiting_queue

        Ok(())
    }

    /// Processes a deferred thread. Returns true if the thread is still running and should be readded to the list of deferred tasks
    async fn process_deferred_thread(&self, thread_info: DeferredThread) {
        /*
            if coroutine.status(data.thread) ~= "dead" then
               resume_with_error_check(data.thread, table.unpack(data.args))
            end
        */
        match thread_info.thread.thread.status() {
            mlua::ThreadStatus::Error | mlua::ThreadStatus::Finished => {}
            _ => {
                //log::debug!("Trying to resume deferred thread");
                let result = self
                    .resume_thread(
                        "DeferredThread",
                        thread_info.thread.thread.clone(),
                        thread_info.thread.args,
                    )
                    .await;

                self.inner.feedback.on_response(
                    "DeferredThread",
                    self,
                    &thread_info.thread.thread,
                    result.as_ref(),
                );
            }
        }
    }

    /// Processes a waiting thread
    async fn process_waiting_thread(&self, thread_info: WaitingThread) -> Option<WaitingThread> {
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
        match thread_info.thread.thread.status() {
            mlua::ThreadStatus::Error | mlua::ThreadStatus::Finished => None,
            _ => {
                let start = thread_info.start;
                let duration = thread_info.duration;
                let current_time = std::time::Instant::now();

                if current_time - start >= duration {
                    log::debug!(
                        "Resuming waiting thread, start: {:?}, duration: {:?}, current_time: {:?}",
                        start,
                        duration,
                        current_time
                    );
                    // resume_with_error_check(thread, table.unpack(data, 1, data.n))
                    let mut args = thread_info.thread.args;
                    args.push_back(mlua::Value::Number((current_time - start).as_secs_f64()));
                    let result = self
                        .resume_thread("WaitingThread", thread_info.thread.thread.clone(), args)
                        .await;

                    self.inner.feedback.on_response(
                        "WaitingThread",
                        self,
                        &thread_info.thread.thread,
                        result.as_ref(),
                    );

                    None
                } else {
                    // Put thread back in queue
                    Some(thread_info)
                }
            }
        }
    }

    async fn process_async_thread(
        &self,
        thread_info: AsyncThreadInfo,
    ) -> Result<Option<AsyncThreadInfo>, mlua::Error> {
        let mut thread_info = thread_info;

        let fut = &mut thread_info.callback;
        let result = futures_util::poll!(fut);

        match result {
            Poll::Pending => Ok(Some(thread_info)),
            Poll::Ready(result) => {
                self.inner.feedback.on_response(
                    "AsyncThread",
                    self,
                    &thread_info.thread,
                    Some(&result),
                );

                match result {
                    Ok(result) => {
                        let result = self
                            .resume_thread("AsyncThread", thread_info.thread.clone(), result)
                            .await;

                        self.inner.feedback.on_response(
                            "AsyncThread.Resume",
                            self,
                            &thread_info.thread,
                            result.as_ref(),
                        );

                        Ok(None)
                    }
                    Err(e) => {
                        let mut result = mlua::MultiValue::new();
                        result.push_back(
                            self.inner
                                .lua
                                .app_data_ref::<mlua::Value>()
                                .unwrap()
                                .clone(),
                        );
                        result.push_back(e.to_string().into_lua(&self.inner.lua)?);

                        let result = self
                            .resume_thread("AsyncThread", thread_info.thread.clone(), result)
                            .await;

                        self.inner.feedback.on_response(
                            "AsyncThread",
                            self,
                            &thread_info.thread,
                            result.as_ref(),
                        );

                        Ok(None)
                    }
                }
            }
        }
    }

    /// Stops the task manager
    pub fn stop(&self) {
        self.inner.is_running.store(false, Ordering::Relaxed);
    }

    /// Clears the task manager queues completely
    pub fn clear(&self) {
        self.inner.async_queue.borrow_mut().clear();
        self.inner.waiting_queue.borrow_mut().clear();
        self.inner.deferred_queue.borrow_mut().clear();
    }

    /// Returns the waiting queue length
    pub fn waiting_len(&self) -> usize {
        self.inner.waiting_queue.borrow().len()
    }

    /// Returns the deferred queue length
    pub fn deferred_len(&self) -> usize {
        self.inner.deferred_queue.borrow_mut().len()
    }

    /// Returns the pending count length
    pub fn pending_len(&self) -> usize {
        self.inner.pending_threads_count.load(Ordering::Relaxed) as usize
    }

    /// Returns the async queue length
    pub fn async_len(&self) -> usize {
        self.inner.async_queue.borrow().len()
    }

    /// Returns the number of items in the whole queue
    pub fn len(&self) -> usize {
        self.waiting_len() + self.deferred_len() + self.pending_len() + self.async_len()
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
