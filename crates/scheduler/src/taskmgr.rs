use futures_util::StreamExt;

use crate::XRc;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

pub struct WaitingThread {
    thread: XRc<ThreadInfo>,
    start: std::time::Instant,
    duration: std::time::Duration,
}

pub struct DeferredThread {
    thread: XRc<ThreadInfo>,
}

#[derive(Debug)]
pub struct ThreadInfo {
    pub thread: mlua::Thread,
    pub args: mlua::MultiValue,
}

pub trait SchedulerFeedback {
    /// Function that is called when any response, Ok or Error occurs
    ///
    /// Returning an error here will drop the task, *not* the task manager
    fn on_response(
        &self,
        label: &str,
        tm: &TaskManager,
        th: &mlua::Thread,
        result: Result<mlua::MultiValue, mlua::Error>,
    ) -> mlua::Result<()>;
}

/// Inner task manager state
pub struct TaskManagerInner {
    pending_threads_count: XRc<AtomicU64>,
    waiting_queue: Mutex<VecDeque<XRc<WaitingThread>>>,
    deferred_queue: Mutex<VecDeque<XRc<DeferredThread>>>,
    is_running: AtomicBool,
    feedback: XRc<dyn SchedulerFeedback>,
}

#[derive(Clone)]
/// Task Manager
pub struct TaskManager {
    inner: XRc<TaskManagerInner>,
}

impl TaskManager {
    pub fn new(feedback: XRc<dyn SchedulerFeedback>) -> Self {
        Self {
            inner: TaskManagerInner {
                pending_threads_count: XRc::new(AtomicU64::new(0)),
                waiting_queue: Mutex::new(VecDeque::default()),
                deferred_queue: Mutex::new(VecDeque::default()),
                is_running: AtomicBool::new(false),
                feedback,
            }
            .into(),
        }
    }

    /// Returns whether the task manager is running
    pub fn is_running(&self) -> bool {
        self.inner.is_running.load(Ordering::Acquire)
    }

    /// Resumes a thread to next
    pub async fn resume_thread(
        &self,
        label: &str,
        thread: mlua::Thread,
        args: mlua::MultiValue,
    ) -> mlua::Result<mlua::MultiValue> {
        log::debug!("StartResumeThread: {}", label);

        self.inner
            .pending_threads_count
            .fetch_add(1, Ordering::Relaxed);

        let mut async_thread = thread.clone().into_async::<mlua::MultiValue>(args);

        let Some(next) = async_thread.next().await else {
            return Ok(mlua::MultiValue::new());
        };

        self.inner
            .pending_threads_count
            .fetch_sub(1, Ordering::Relaxed);

        tokio::task::yield_now().await;
        log::debug!("EndResumeThread {}", label);

        next
    }

    /// Adds a waiting thread to the task manager
    pub fn add_waiting_thread(
        &self,
        thread: mlua::Thread,
        args: mlua::MultiValue,
        duration: std::time::Duration,
    ) {
        log::debug!("Trying to add thread to waiting queue");
        let tinfo = XRc::new(ThreadInfo { thread, args });
        let mut self_ref = self.inner.waiting_queue.lock().unwrap();
        self_ref.push_front(XRc::new(WaitingThread {
            thread: tinfo,
            start: std::time::Instant::now(),
            duration,
        }));
        log::debug!("Added thread to waiting queue");
    }

    /// Adds a deferred thread to the task manager
    pub fn add_deferred_thread(&self, thread: mlua::Thread, args: mlua::MultiValue) {
        log::debug!("Adding deferred thread to queue");
        let tinfo = XRc::new(ThreadInfo { thread, args });
        let mut self_ref = self.inner.deferred_queue.lock().unwrap();
        self_ref.push_front(XRc::new(DeferredThread { thread: tinfo }));
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

    pub async fn process(&self) -> Result<(), mlua::Error> {
        /*log::debug!(
            "Queue Length: {}, Running: {}",
            self.inner.len(),
            self.inner.is_running()
        );*/

        // Process all threads in the queue

        //log::debug!("Queue Length After Defer: {}", self.inner.len());
        let mut readd_wait_list = Vec::new();
        {
            loop {
                // Pop element from self_ref
                let mut self_ref = self.inner.waiting_queue.lock().unwrap();

                let Some(entry) = self_ref.pop_back() else {
                    break;
                };

                drop(self_ref);

                if self.process_waiting_thread(&entry).await? {
                    readd_wait_list.push(entry);
                }
            }
        }

        let mut readd_deferred_list = Vec::new();
        {
            loop {
                // Pop element from self_ref
                let mut self_ref = self.inner.deferred_queue.lock().unwrap();

                let Some(entry) = self_ref.pop_back() else {
                    break;
                };

                drop(self_ref);

                if self.process_deferred_thread(&entry).await? {
                    readd_deferred_list.push(entry);
                }
            }
        }

        if !readd_deferred_list.is_empty() {
            // Readd threads that need to be re-added
            let mut self_ref = self.inner.deferred_queue.lock().unwrap();
            for entry in readd_deferred_list {
                self_ref.push_back(entry);
            }

            drop(self_ref);
        }

        if !readd_wait_list.is_empty() {
            // Readd threads that need to be re-added
            let mut self_ref = self.inner.waiting_queue.lock().unwrap();
            for entry in readd_wait_list {
                self_ref.push_back(entry);
            }

            drop(self_ref);
        }

        // Check all_threads, removing all finished threads and resuming all threads not in deferred_queue or waiting_queue

        Ok(())
    }

    /// Processes a deferred thread. Returns true if the thread is still running and should be readded to the list of deferred tasks
    async fn process_deferred_thread(
        &self,
        thread_info: &XRc<DeferredThread>,
    ) -> mlua::Result<bool> {
        /*
            if coroutine.status(data.thread) ~= "dead" then
               resume_with_error_check(data.thread, table.unpack(data.args))
            end
        */
        match thread_info.thread.thread.status() {
            mlua::ThreadStatus::Error | mlua::ThreadStatus::Finished => Ok(false),
            _ => {
                //log::debug!("Trying to resume deferred thread");
                let result = self
                    .resume_thread(
                        "DeferredThread",
                        thread_info.thread.thread.clone(),
                        thread_info.thread.args.clone(),
                    )
                    .await;

                self.inner.feedback.on_response(
                    "DeferredThread",
                    self,
                    &thread_info.thread.thread,
                    result,
                )?;

                Ok(false)
            }
        }
    }

    /// Processes a waiting thread
    async fn process_waiting_thread(&self, thread_info: &XRc<WaitingThread>) -> mlua::Result<bool> {
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
            mlua::ThreadStatus::Error | mlua::ThreadStatus::Finished => Ok(false),
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
                    let result = self
                        .resume_thread(
                            "WaitingThread",
                            thread_info.thread.thread.clone(),
                            thread_info.thread.args.clone(),
                        )
                        .await;

                    self.inner.feedback.on_response(
                        "WaitingThread",
                        self,
                        &thread_info.thread.thread,
                        result,
                    )?;
                    Ok(false)
                } else {
                    // Put thread back in queue
                    Ok(true)
                }
            }
        }
    }

    /// Stops the task manager
    pub fn stop(&self) {
        self.inner.is_running.store(false, Ordering::Relaxed);
    }

    /// Returns the waiting queue length
    pub fn waiting_len(&self) -> usize {
        self.inner.waiting_queue.lock().unwrap().len()
    }

    /// Returns the deferred queue length
    pub fn deferred_len(&self) -> usize {
        self.inner.deferred_queue.lock().unwrap().len()
    }

    /// Returns the pending count length
    pub fn pending_len(&self) -> usize {
        self.inner.pending_threads_count.load(Ordering::Relaxed) as usize
    }

    /// Returns the number of items in the whole queue
    pub fn len(&self) -> usize {
        self.waiting_len() + self.deferred_len() + self.pending_len()
    }

    /// Returns if the queue is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub async fn wait_till_done(&self, sleep_interval: Duration) {
        while self.is_running() {
            if self.is_empty() {
                break;
            }

            tokio::time::sleep(sleep_interval).await;
        }
    }
}

pub fn add_scheduler(
    lua: &mlua::Lua,
    feedback: XRc<dyn SchedulerFeedback>,
) -> mlua::Result<XRc<TaskManager>> {
    let task_manager = XRc::new(TaskManager::new(feedback));
    lua.set_app_data(XRc::clone(&task_manager));
    Ok(task_manager)
}

pub fn get(lua: &mlua::Lua) -> XRc<TaskManager> {
    lua.app_data_ref::<XRc<TaskManager>>()
        .expect("Failed to get task manager")
        .clone()
}
