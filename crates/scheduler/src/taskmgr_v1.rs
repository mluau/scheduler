use crate::{XRc, XRefCell, XBool, XUsize};
use std::cell::Cell;
use std::collections::{BinaryHeap, VecDeque};
use std::time::Duration;
use crate::taskmgr::SchedulerFeedback;

pub enum WaitOp {
    // task.wait semantics
    Wait,
    // task.delay semantics
    Delay { 
        args: mlua::MultiValue,
    },
}

pub struct WaitingThread {
    pub thread: mlua::Thread,
    pub op: WaitOp,
    pub start: std::time::Instant,
    pub wake_at: std::time::Instant,
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
    pub thread: mlua::Thread,
    pub args: mlua::MultiValue,
}

/// Inner v1 task manager state
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

impl TaskManagerInner {
    pub(crate) fn new(
        lua: &mlua::Lua,
        feedback: XRc<dyn SchedulerFeedback>,
    ) -> Self {
        Self {
            pending_asyncs: XUsize::new(0),
            waiting_queue: XRefCell::new(BinaryHeap::default()),
            deferred_queue: XRefCell::new(VecDeque::default()),
            is_running: XBool::new(false),
            is_cancelled: XBool::new(false),
            feedback,
            lua: lua.weak(),
            async_task_executor: XRefCell::new(tokio::task::JoinSet::new()),
        }
    }

    pub(crate) async fn run(&self, ticker: tokio::sync::broadcast::Receiver<()>) {
        if self.is_running() || self.is_cancelled() || !self.check_lua() {
            return; // Quick exit
        }

        let mut ticker = ticker;

        self.is_running.set(true);

        log::debug!("Task manager started");

        loop {
            if self.is_cancelled() || !self.check_lua() {
                break;
            }

            self.process().await;

            let _ = ticker.recv().await;
        }

        self.is_running.set(false)
    }


    /// Processes the task manager
    async fn process(&self) {
        // Process all threads in the queue

        //log::debug!("Queue Length After Defer: {}", self.inner.len());
        {
            loop {
                let current_time = std::time::Instant::now();

                // Pop element from self_ref
                let entry = {
                    let mut self_ref = self.waiting_queue.borrow_mut();

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
                    let mut self_ref = self.deferred_queue.borrow_mut();

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
                    self.feedback.on_response(
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
                            self.feedback.on_response(
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

                        self.feedback.on_response(
                            "DelayedThread",
                            &thread_info.thread,
                            result,
                        );
                    }
                };
            }
        }
    }

    /// Checks if the lua state is valid
    fn check_lua(&self) -> bool {
        self.lua.try_upgrade().is_some()
    }

    /// Returns whether the task manager has been cancelled
    fn is_cancelled(&self) -> bool {
        self.is_cancelled.get()
    }

    /// Returns whether the task manager is running
    fn is_running(&self) -> bool {
        self.is_running.get()
    }
}