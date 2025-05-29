use tokio_util::time::DelayQueue;
use tokio_util::time::delay_queue::Key as DelayQueueKey;
use crate::{XRc, XBool, XRefCell, XId, XUsize};
use std::collections::{HashMap, VecDeque};
use futures_util::stream::StreamExt;
use mlua::IntoLuaMulti;
use mlua::prelude::LuaResult;
use crate::taskmgr::SchedulerFeedback;

pub struct WaitingThread {
    delay_args: Option<mlua::MultiValue>,
    start_at: std::time::Instant,
    thread: mlua::Thread,
}

pub struct DeferredThread {
    thread: mlua::Thread,
    args: mlua::MultiValue,
}

pub enum SchedulerEvent {
    // task.wait / task.delay semantics
    Wait {
        delay_args: Option<mlua::MultiValue>,
        thread: mlua::Thread,
        start_at: std::time::Instant,
        duration: std::time::Duration
    },
    DeferredThread {
        thread: mlua::Thread,
        args: mlua::MultiValue,
    },
    CancelWaitThread {
        xid: crate::XId,
    },
    CancelDeferredThread {
        xid: crate::XId,
    },
    Clear {}
}

pub struct CoreScheduler {
    pub lua: mlua::WeakLua,
    pub feedback: XRc<dyn SchedulerFeedback>,

    // Status flags
    pub is_running: XBool,
    pub is_cancelled: XBool,

    tx: tokio::sync::mpsc::UnboundedSender<SchedulerEvent>,
    rx: XRefCell<Option<tokio::sync::mpsc::UnboundedReceiver<SchedulerEvent>>>,

    // v1 compat
    pub pending_asyncs: XUsize,
    pub async_task_executor: XRefCell<tokio::task::JoinSet<()>>,
}

impl CoreScheduler {
    /// Creates a new task manager
    pub fn new(
        lua: mlua::WeakLua,
        feedback: XRc<dyn SchedulerFeedback>,
    ) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self { 
            lua,
            feedback,
            is_running: XBool::new(false),
            is_cancelled: XBool::new(false),
            tx,
            rx: XRefCell::new(Some(rx)),
            pending_asyncs: XUsize::new(0),
            async_task_executor: XRefCell::new(tokio::task::JoinSet::new()),
        }
    }

    /// Runs the task manager
    pub(crate) async fn run(&self) {
        let mut rx = self.rx.borrow_mut().take().expect("No reciever found");

        let mut wait_queue: DelayQueue<WaitingThread> = DelayQueue::new();
        let mut wait_keys: HashMap<XId, Vec<DelayQueueKey>> = HashMap::new();
        let mut deferred_queue: VecDeque<DeferredThread> = VecDeque::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));

        if self.is_running() || self.is_cancelled() || !self.check_lua() {
            return; // Quick exit
        }

        loop {
            if self.is_cancelled() || !self.check_lua() {
                self.is_running.set(false);
                return;
            }


            tokio::select! {
                Some(event) = rx.recv() => {
                    match event {
                        SchedulerEvent::Wait { delay_args, thread, start_at, duration } => {
                            let xid = XId::from_ptr(thread.to_pointer());
                            let key = wait_queue.insert(
                                WaitingThread {
                                    delay_args,
                                    start_at,
                                    thread,
                                },
                                duration
                            );
                            wait_keys.entry(xid).or_default().push(key);
                        },
                        SchedulerEvent::DeferredThread { thread, args } => {
                            let xid = XId::from_ptr(thread.to_pointer());
                            deferred_queue.push_back(DeferredThread {
                                thread,
                                args
                            });
                        },
                        SchedulerEvent::CancelWaitThread { xid } => {
                            if let Some(keys) = wait_keys.get(&xid) {
                                for key in keys {
                                    wait_queue.remove(key);
                                }
                                wait_keys.remove(&xid);
                            };
                        }
                        SchedulerEvent::CancelDeferredThread { xid } => {
                            deferred_queue.retain(|x| {
                                if crate::XId::from_ptr(x.thread.to_pointer()) != xid {
                                    true
                                } else {
                                    false
                                }
                            });
                        }
                        SchedulerEvent::Clear {} => {
                            deferred_queue.clear();
                            wait_queue.clear();
                            wait_keys.clear();
                        }
                    }
                },
                _ = interval.tick() => {
                    if self.is_cancelled() || !self.check_lua() {
                        self.is_running.set(false);
                        return;
                    }

                    if !wait_queue.is_empty() {
                        // Flush out the wait queue
                        let current_time = std::time::Instant::now();
                        while let Some(value) = wait_queue.next().await {
                            let Some(lua) = self.lua.try_upgrade() else {
                                return
                            };

                            let key = value.key();
                            let inner = value.into_inner();
                            let xid = XId::from_ptr(inner.thread.to_pointer());
                            if let Err(e) = self.process_waiting_thread(&lua, inner, current_time).await {
                                log::error!("Wait queue fail: {:?}", e);
                            }

                            if wait_keys.contains_key(&xid) {
                                if let Some(old_keys) = wait_keys.remove(&xid) {
                                    let mut new_keys = Vec::with_capacity(old_keys.len());
                                    for nkey in old_keys {
                                        if nkey == key {
                                            continue;
                                        }

                                        new_keys.push(nkey);
                                    }

                                    wait_keys.insert(xid, new_keys);
                                }
                            }
                        }
                    }

                    if !deferred_queue.is_empty() {
                        while let Some(value) = deferred_queue.pop_back() {
                            if self.is_cancelled() || !self.check_lua() {
                                self.is_running.set(false);
                                break;
                            }

                            self.process_deferred_thread(value).await;
                        }
                    }
                }
            }
        }
    }

    /// Processes a waiting thread
    async fn process_waiting_thread(
        &self,
        lua: &mlua::Lua,
        thread_info: WaitingThread,
        current_time: std::time::Instant,
    ) -> LuaResult<()> {
        match thread_info.thread.status() {
            mlua::ThreadStatus::Error | mlua::ThreadStatus::Finished => {}
            _ => {
                let args = match thread_info.delay_args {
                    Some(args) => args,
                    None => {
                        (current_time - thread_info.start_at).as_secs_f64().into_lua_multi(lua)?
                    }
                };

                {
                    let r = thread_info
                        .thread
                        .resume(args);
                    //tokio::task::yield_now().await;

                    self.feedback.on_response(
                        "WaitingThread",
                        &thread_info.thread,
                        r,
                    );        
                }
            }
        }

        Ok(())
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
                    //tokio::task::yield_now().await;
                    self.feedback.on_response(
                        "DeferredThread",
                        &thread_info.thread,
                        r,
                    );    
                };
            }
        }
    }

    /// Checks if the lua state is valid
    fn check_lua(&self) -> bool {
        self.lua.try_upgrade().is_some()
    }

    /// Returns whether the task manager has been cancelled
    pub fn is_cancelled(&self) -> bool {
        self.is_cancelled.get()
    }

    /// Returns whether the task manager is running
    pub fn is_running(&self) -> bool {
        self.is_running.get()
    }

    /// Stops the task manager
    pub fn stop(&self) {
        self.is_cancelled.set(true);
    }

    /// Returns the feedback stored in the task manager
    pub fn feedback(&self) -> &dyn SchedulerFeedback {
        &*self.feedback
    }

    /// Adds a waiting thread to the task manager
    pub fn push_event(
        &self,
        event: SchedulerEvent
    ) {
        let _ = self.tx.send(event);
    }
}