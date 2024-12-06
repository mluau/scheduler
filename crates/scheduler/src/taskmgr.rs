use scc::Queue;
use std::{
    rc::Rc,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

pub struct WaitingThread {
    thread: ThreadInfo,
    start: u64,
    resume_ticks: u64,
}

pub struct DeferredThread {
    thread: ThreadInfo,
}

#[derive(Debug)]
pub struct ThreadInfo {
    thread: mlua::Thread,
    args: mlua::MultiValue,
}

pub type OnError = fn(th: &ThreadInfo, mlua::Error) -> mlua::Result<()>;

/// Task Manager
pub struct TaskManager {
    heartbeater: flume::Receiver<()>,
    waiting_queue: std::sync::Mutex<std::collections::VecDeque<Rc<WaitingThread>>>,
    deferred_queue: Queue<Rc<DeferredThread>>,
    is_running: AtomicBool,
    ticks: AtomicU64,

    /// Function that is called when an error occurs
    ///
    /// If this function returns an error, the task manager will stop and error with the error
    on_error: OnError,
}

impl TaskManager {
    pub fn new(heartbeater: flume::Receiver<()>, on_error: OnError) -> Self {
        Self {
            heartbeater,
            waiting_queue: std::sync::Mutex::new(std::collections::VecDeque::default()),
            deferred_queue: Queue::default(),
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
        //println!("Trying to add thread to waiting queue");
        let mut self_ref = self.waiting_queue.lock().unwrap();
        self_ref.push_front(Rc::new(WaitingThread {
            thread: ThreadInfo { thread, args },
            start: self.ticks.load(Ordering::Relaxed),
            resume_ticks,
        }));
        //println!("Added thread to waiting queue");
    }

    /// Adds a deferred thread to the task manager
    pub fn add_deferred_thread(&self, thread: mlua::Thread, args: mlua::MultiValue) {
        self.deferred_queue.push(Rc::new(DeferredThread {
            thread: ThreadInfo { thread, args },
        }));
    }

    /// Runs the task manager
    pub async fn run(&self) -> Result<(), mlua::Error> {
        self.is_running.store(true, Ordering::Relaxed);

        //println!("Task Manager started");

        loop {
            if !self.is_running() {
                break;
            }

            self.process()?;

            // Wait for next heartbeat to continue processing
            match self.heartbeater.recv_async().await {
                Ok(_) => {}
                Err(flume::RecvError::Disconnected) => {
                    break;
                }
            }

            // Increment tick count
            self.ticks.fetch_add(1, Ordering::Acquire);

            //println!("Tick: {}", self.ticks.load(Ordering::Acquire));
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
        let mut readd_list = Vec::new();
        while let Some(entry) = self.deferred_queue.pop() {
            if self.process_deferred_thread(&entry)? {
                readd_list.push((entry).clone());
            }
        }

        // Readd threads that need to be readded
        for entry in readd_list {
            let rc: &Rc<DeferredThread> = &entry;
            self.deferred_queue.push(rc.clone());
        }

        //println!("Queue Length After Defer: {}", self.len());
        let mut readd_list = Vec::new();
        {
            loop {
                // Pop element from self_ref
                let mut self_ref = self.waiting_queue.lock().unwrap();

                let Some(entry) = self_ref.pop_back() else {
                    break;
                };

                drop(self_ref);

                if self.process_waiting_thread(&entry)? {
                    readd_list.push(entry);
                }
            }

            // Readd threads that need to be re-added
            let mut self_ref = self.waiting_queue.lock().unwrap();
            for entry in readd_list {
                self_ref.push_back(entry);
            }

            drop(self_ref);

            //println!("Mutex unlocked");
        }

        //println!("Queue Length: {}", self.len());

        //println!("Tick: {}", self.ticks.load(Ordering::Acquire));

        Ok(())
    }

    /// Processes a deferred thread
    fn process_deferred_thread(&self, thread_info: &Rc<DeferredThread>) -> mlua::Result<bool> {
        /*
            if coroutine.status(data.thread) ~= "dead" then
               resume_with_error_check(data.thread, table.unpack(data.args))
            end
        */
        match thread_info.thread.thread.status() {
            mlua::ThreadStatus::Error => {}
            _ => {
                match thread_info
                    .thread
                    .thread
                    .resume::<mlua::MultiValue>(thread_info.thread.args.clone())
                {
                    Ok(_) => {}
                    Err(err) => {
                        (self.on_error)(&thread_info.thread, err)?;
                    }
                }
            }
        }

        Ok(false)
    }

    /// Processes a waiting thread
    fn process_waiting_thread(&self, thread_info: &Rc<WaitingThread>) -> mlua::Result<bool> {
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
        match thread_info.thread.thread.status() {
            mlua::ThreadStatus::Error => {}
            _ => {
                let ticks = self.ticks.load(Ordering::Relaxed);
                //println!("Ticks: {}, Resume Ticks: {}", ticks, resume_ticks + start);
                if ticks > (resume_ticks + start) {
                    // resume_with_error_check(thread, table.unpack(data, 1, data.n))
                    match thread_info
                        .thread
                        .thread
                        .resume::<mlua::MultiValue>(thread_info.thread.args.clone())
                    {
                        Ok(_) => {}
                        Err(err) => {
                            (self.on_error)(&thread_info.thread, err)?;
                        }
                    }
                } else {
                    // Put thread back in queue
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Stops the task manager
    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
    }

    /// Returns the number of items in the whole queue
    pub fn len(&self) -> usize {
        let self_ref = self.waiting_queue.lock().unwrap();
        self.deferred_queue.len() + self_ref.len()
    }

    /// Returns if the queue is empty
    pub fn is_empty(&self) -> bool {
        let self_ref = self.waiting_queue.lock().unwrap();
        self.deferred_queue.is_empty() && self_ref.is_empty()
    }
}

pub fn add_scheduler(
    lua: &mlua::Lua,
    hb: flume::Receiver<()>,
    on_error: OnError,
) -> mlua::Result<Rc<TaskManager>> {
    let task_manager = Rc::new(TaskManager::new(hb, on_error));
    lua.set_app_data(Rc::clone(&task_manager));
    Ok(task_manager)
}

pub fn get(lua: &mlua::Lua) -> Rc<TaskManager> {
    lua.app_data_ref::<Rc<TaskManager>>()
        .expect("Failed to get task manager")
        .clone()
}
