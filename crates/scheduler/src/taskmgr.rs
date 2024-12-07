use crate::XRc;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

pub struct WaitingThread {
    thread: XRc<ThreadInfo>,
    start: u64,
    resume_ticks: u64,
}

pub struct DeferredThread {
    thread: XRc<ThreadInfo>,
}

#[derive(Debug)]
pub struct ThreadInfo {
    thread: mlua::Thread,
    args: mlua::MultiValue,
}

pub type OnError = fn(th: &ThreadInfo, mlua::Error) -> mlua::Result<()>;

/// Task Manager
pub struct TaskManager {
    pending_threads: Mutex<Vec<XRc<ThreadInfo>>>,
    waiting_queue: Mutex<VecDeque<XRc<WaitingThread>>>,
    deferred_queue: Mutex<VecDeque<XRc<DeferredThread>>>,
    is_running: AtomicBool,
    ticks: AtomicU64,

    /// Function that is called when an error occurs
    ///
    /// If this function returns an error, the task manager will stop and error with the error
    on_error: OnError,
}

impl TaskManager {
    pub fn new(on_error: OnError) -> Self {
        Self {
            waiting_queue: Mutex::new(VecDeque::default()),
            deferred_queue: Mutex::new(VecDeque::default()),
            pending_threads: Mutex::new(Vec::default()),
            is_running: AtomicBool::new(false),
            ticks: AtomicU64::new(0),
            on_error,
        }
    }

    /// Returns whether the task manager is running
    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::Acquire)
    }

    /// Adds a thread to the task manager
    pub fn add_waiting_thread(
        &self,
        thread: mlua::Thread,
        args: mlua::MultiValue,
        resume_ticks: u64,
    ) {
        /*println!(
            "Adding waiting thread with curTicks: {}, resumeTicks: {}",
            self.ticks.load(Ordering::Relaxed),
            resume_ticks
        );*/
        //println!("Trying to add thread to waiting queue");
        let tinfo = XRc::new(ThreadInfo { thread, args });
        let mut self_ref = self.waiting_queue.lock().unwrap();
        self_ref.push_front(XRc::new(WaitingThread {
            thread: tinfo,
            start: self.ticks.load(Ordering::Relaxed),
            resume_ticks,
        }));
        //println!("Added thread to waiting queue");
    }

    /// Adds a deferred thread to the task manager
    pub fn add_deferred_thread(&self, thread: mlua::Thread, args: mlua::MultiValue) {
        let tinfo = XRc::new(ThreadInfo { thread, args });
        let mut self_ref = self.deferred_queue.lock().unwrap();
        self_ref.push_front(XRc::new(DeferredThread { thread: tinfo }));
    }

    /// Runs the task manager
    pub async fn run(&self, heartbeater: flume::Receiver<()>) -> Result<(), mlua::Error> {
        self.is_running.store(true, Ordering::Relaxed);

        //println!("Task Manager started");

        loop {
            if !self.is_running() {
                break;
            }

            if self.is_empty() {
                // Wait for next heartbeat
                match heartbeater.recv_async().await {
                    Ok(_) => {}
                    Err(flume::RecvError::Disconnected) => {
                        break;
                    }
                }

                tokio::task::yield_now().await;
            }

            self.process()?;

            // Wait for next heartbeat to continue processing
            match heartbeater.recv_async().await {
                Ok(_) => {}
                Err(flume::RecvError::Disconnected) => {
                    break;
                }
            }

            // Increment tick count
            self.ticks.fetch_add(1, Ordering::Acquire);
        }

        Ok(())
    }

    pub fn process(&self) -> Result<(), mlua::Error> {
        /*println!(
            "Queue Length: {}, Running: {}",
            self.len(),
            self.is_running()
        );*/

        // Process all threads in the queue
        //
        // NOTE/DEVIATION FROM ROBLOX BEHAVIOUR: All threads are processed in a single event loop per heartbeat

        let mut readd_pending = Vec::new();
        {
            loop {
                // Pop element from self_ref
                let mut self_ref = self.pending_threads.lock().unwrap();

                let Some(entry) = self_ref.pop() else {
                    break;
                };

                drop(self_ref);

                if self.process_pending_thread(&entry)? {
                    readd_pending.push(entry);
                } else {
                    self.add_deferred_thread(entry.thread.clone(), entry.args.clone());
                }
            }
        }

        let mut readd_deferred_list = Vec::new();
        {
            loop {
                // Pop element from self_ref
                let mut self_ref = self.deferred_queue.lock().unwrap();

                let Some(entry) = self_ref.pop_back() else {
                    break;
                };

                drop(self_ref);

                if self.process_deferred_thread(&entry)? {
                    readd_deferred_list.push(entry);
                }
            }
        }

        //println!("Queue Length After Defer: {}", self.len());
        let mut readd_wait_list = Vec::new();
        {
            loop {
                // Pop element from self_ref
                let mut self_ref = self.waiting_queue.lock().unwrap();

                let Some(entry) = self_ref.pop_back() else {
                    break;
                };

                drop(self_ref);

                if self.process_waiting_thread(&entry)? {
                    readd_wait_list.push(entry);
                }
            }
        }

        {
            // Readd threads that need to be re-added
            let mut self_ref = self.pending_threads.lock().unwrap();
            for entry in readd_pending {
                self_ref.push(entry);
            }

            drop(self_ref);

            // Readd threads that need to be re-added
            let mut self_ref = self.deferred_queue.lock().unwrap();
            for entry in readd_deferred_list {
                self_ref.push_back(entry);
            }

            drop(self_ref);

            // Readd threads that need to be re-added
            let mut self_ref = self.waiting_queue.lock().unwrap();
            for entry in readd_wait_list {
                self_ref.push_back(entry);
            }

            drop(self_ref);

            //println!("Mutex unlocked");
        }

        // Check all_threads, removing all finished threads and resuming all threads not in deferred_queue or waiting_queue

        Ok(())
    }

    /// Processes a pending thread. Returns true if the thread is still running and should be readded to the list of pending tasks
    ///
    /// If it returns false, the thread should be deferred normally
    fn process_pending_thread(&self, thread_info: &XRc<ThreadInfo>) -> mlua::Result<bool> {
        /*
            if coroutine.status(data.thread) ~= "dead" then
               resume_with_error_check(data.thread, table.unpack(data.args))
            end
        */
        match thread_info.thread.status() {
            mlua::ThreadStatus::Error | mlua::ThreadStatus::Finished => Ok(false),
            _ => {
                //println!("Trying to resume deferred thread");
                match thread_info
                    .thread
                    .resume::<mlua::Value>(thread_info.args.clone())
                {
                    Ok(res) => {
                        if let mlua::Value::LightUserData(ud) = res {
                            // mlua has a really dumb poll pending invariant to deal with
                            if ud == mlua::Lua::poll_pending() {
                                return Ok(true);
                            }
                        }

                        Ok(false)
                    }
                    Err(err) => {
                        (self.on_error)(thread_info, err)?;
                        Ok(false)
                    }
                }
            }
        }
    }

    /// Processes a deferred thread. Returns true if the thread is still running and should be readded to the list of deferred tasks
    fn process_deferred_thread(&self, thread_info: &XRc<DeferredThread>) -> mlua::Result<bool> {
        /*
            if coroutine.status(data.thread) ~= "dead" then
               resume_with_error_check(data.thread, table.unpack(data.args))
            end
        */
        match thread_info.thread.thread.status() {
            mlua::ThreadStatus::Error | mlua::ThreadStatus::Finished => Ok(false),
            _ => {
                //println!("Trying to resume deferred thread");
                match thread_info
                    .thread
                    .thread
                    .resume::<mlua::Value>(thread_info.thread.args.clone())
                {
                    Ok(res) => {
                        if let mlua::Value::LightUserData(ud) = res {
                            // mlua has a really dumb poll pending invariant to deal with
                            if ud == mlua::Lua::poll_pending() {
                                let mut self_ref = self.pending_threads.lock().unwrap();
                                self_ref.push(thread_info.thread.clone());
                                drop(self_ref);
                                return Ok(false);
                            }
                        }

                        println!("Deferred thread resumed with {:?}", res);

                        Ok(false)
                    }
                    Err(err) => {
                        (self.on_error)(&thread_info.thread, err)?;
                        Ok(false)
                    }
                }
            }
        }
    }

    /// Processes a waiting thread
    fn process_waiting_thread(&self, thread_info: &XRc<WaitingThread>) -> mlua::Result<bool> {
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
        let start = thread_info.start;
        let resume_ticks = thread_info.resume_ticks;
        //println!("Thread Status: {:?}", thread_info.thread.thread.status());
        match thread_info.thread.thread.status() {
            mlua::ThreadStatus::Error | mlua::ThreadStatus::Finished => Ok(false),
            mlua::ThreadStatus::Running => Ok(true),
            mlua::ThreadStatus::Resumable => {
                let ticks = self.ticks.load(Ordering::Relaxed);
                if ticks > (resume_ticks + start) {
                    /*println!(
                        "Resuming waiting thread, ticks: {}, resume_ticks: {}, start: {}",
                        ticks, resume_ticks, start
                    );*/
                    // resume_with_error_check(thread, table.unpack(data, 1, data.n))
                    match thread_info
                        .thread
                        .thread
                        .resume::<mlua::Value>(thread_info.thread.args.clone())
                    {
                        Ok(res) => {
                            if let mlua::Value::LightUserData(ud) = res {
                                // mlua has a really dumb poll pending invariant to deal with
                                if ud == mlua::Lua::poll_pending() {
                                    let mut self_ref = self.pending_threads.lock().unwrap();
                                    self_ref.push(thread_info.thread.clone());
                                    drop(self_ref);
                                    return Ok(false);
                                }
                            }

                            Ok(false)
                        }
                        Err(err) => {
                            (self.on_error)(&thread_info.thread, err)?;
                            Ok(false)
                        }
                    }
                } else {
                    // Put thread back in queue
                    Ok(true)
                }
            }
        }
    }

    /// Stops the task manager
    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
    }

    /// Returns the waiting queue length
    pub fn waiting_len(&self) -> usize {
        self.waiting_queue.lock().unwrap().len()
    }

    /// Returns the deferred queue length
    pub fn deferred_len(&self) -> usize {
        self.deferred_queue.lock().unwrap().len()
    }

    /// Returns the pending threads length
    pub fn pending_len(&self) -> usize {
        self.pending_threads.lock().unwrap().len()
    }

    /// Returns the number of items in the whole queue
    pub fn len(&self) -> usize {
        self.waiting_len() + self.deferred_len() + self.pending_len()
    }

    /// Returns if the queue is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

pub fn add_scheduler(lua: &mlua::Lua, on_error: OnError) -> mlua::Result<XRc<TaskManager>> {
    let task_manager = XRc::new(TaskManager::new(on_error));
    lua.set_app_data(XRc::clone(&task_manager));
    Ok(task_manager)
}

pub fn get(lua: &mlua::Lua) -> XRc<TaskManager> {
    lua.app_data_ref::<XRc<TaskManager>>()
        .expect("Failed to get task manager")
        .clone()
}
