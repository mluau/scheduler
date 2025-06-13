use crate::taskmgr::SchedulerFeedback;
use crate::{XBool, XId, XRc, XRefCell};
use futures_util::stream::FuturesUnordered;
use futures_util::Future;
use futures_util::FutureExt;
use futures_util::StreamExt;
use futures_util::TryFutureExt;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::watch::{Receiver, Sender};
use tokio_util::time::delay_queue::Key as DelayQueueKey;
use tokio_util::time::DelayQueue;

pub struct WaitingThread {
    delay_args: Option<mlua::MultiValue>,
    start_at: std::time::Instant,
    thread: mlua::Thread,
}

pub struct DeferredThread {
    thread: mlua::Thread,
    args: mlua::MultiValue,
    xid: crate::XId,
}

pub enum SchedulerEvent {
    // task.wait / task.delay semantics
    Wait {
        delay_args: Option<mlua::MultiValue>,
        thread: mlua::Thread,
        start_at: std::time::Instant,
        duration: std::time::Duration,
    },
    DeferredThread {
        thread: mlua::Thread,
        args: mlua::MultiValue,
    },
    AddAsync {
        thread: mlua::Thread,
        #[cfg(feature = "send")]
        fut: Pin<Box<dyn Future<Output = mlua::Result<mlua::MultiValue>> + Send + Sync>>,
        #[cfg(not(feature = "send"))]
        fut: Pin<Box<dyn Future<Output = mlua::Result<mlua::MultiValue>>>>,
    },
    Clear {},
    Close {},
}

#[cfg(feature = "v2_taskmgr_flume")]
/// Struct to allow flume to expose the needed async channel methods needed for v2 task scheduler
struct InnerFlumeRecv<T>(flume::Receiver<T>);

#[cfg(feature = "v2_taskmgr_flume")]
impl<T> InnerFlumeRecv<T> {
    async fn recv(&self) -> Option<T> {
        match self.0.recv_async().await {
            Ok(t) => Some(t),
            Err(_) => None,
        }
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Inner scheduler v2
pub struct CoreScheduler {
    pub lua: mlua::WeakLua,
    pub feedback: XRc<dyn SchedulerFeedback>,

    // Status flags
    pub is_running: XBool,
    pub is_cancelled: XBool,

    // tx/rx
    #[cfg(not(feature = "v2_taskmgr_flume"))]
    tx: UnboundedSender<SchedulerEvent>,
    #[cfg(not(feature = "v2_taskmgr_flume"))]
    rx: XRefCell<Option<UnboundedReceiver<SchedulerEvent>>>,
    #[cfg(feature = "v2_taskmgr_flume")]
    tx: flume::Sender<SchedulerEvent>,
    #[cfg(feature = "v2_taskmgr_flume")]
    rx: XRefCell<Option<InnerFlumeRecv<SchedulerEvent>>>,

    // Cancellation channel
    #[cfg(not(feature = "v2_taskmgr_flume"))]
    cancel_tx: UnboundedSender<crate::XId>,
    #[cfg(not(feature = "v2_taskmgr_flume"))]
    cancel_rx: XRefCell<Option<UnboundedReceiver<crate::XId>>>,
    #[cfg(feature = "v2_taskmgr_flume")]
    cancel_tx: flume::Sender<crate::XId>,
    #[cfg(feature = "v2_taskmgr_flume")]
    cancel_rx: XRefCell<Option<InnerFlumeRecv<crate::XId>>>,

    // Cancellation channel
    #[cfg(not(feature = "v2_taskmgr_flume"))]
    cancel: XRefCell<HashSet<XId>>,

    done_tx: Sender<bool>,
    done_rx: tokio::sync::RwLock<Receiver<bool>>,
}

impl CoreScheduler {
    /// Creates a new task manager
    pub fn new(lua: mlua::WeakLua, feedback: XRc<dyn SchedulerFeedback>) -> Self {
        #[cfg(not(feature = "v2_taskmgr_flume"))]
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        #[cfg(feature = "v2_taskmgr_flume")]
        let (tx, rx) = flume::unbounded();

        #[cfg(not(feature = "v2_taskmgr_flume"))]
        let (cancel_tx, cancel_rx) = tokio::sync::mpsc::unbounded_channel();
        #[cfg(feature = "v2_taskmgr_flume")]
        let (cancel_tx, cancel_rx) = flume::unbounded();

        let (done_tx, done_rx) = tokio::sync::watch::channel(true);
        Self {
            lua,
            feedback,
            is_running: XBool::new(false),
            is_cancelled: XBool::new(false),

            // TX/RX
            tx,
            #[cfg(not(feature = "v2_taskmgr_flume"))]
            rx: XRefCell::new(Some(rx)),
            #[cfg(feature = "v2_taskmgr_flume")]
            rx: XRefCell::new(Some(InnerFlumeRecv(rx))),

            // CANCELLATION TX/RX
            cancel_tx,
            #[cfg(not(feature = "v2_taskmgr_flume"))]
            cancel_rx: XRefCell::new(Some(cancel_rx)),
            #[cfg(feature = "v2_taskmgr_flume")]
            cancel_rx: XRefCell::new(Some(InnerFlumeRecv(cancel_rx))),

            cancel: XRefCell::new(HashSet::new()),
            done_tx,
            done_rx: tokio::sync::RwLock::new(done_rx),
        }
    }

    /// Runs the task manager
    pub(crate) async fn run(&self) {
        let mut rx = self.rx.borrow_mut().take().expect("No reciever found");
        let mut cancel_rx = self
            .cancel_rx
            .borrow_mut()
            .take()
            .expect("No cancel reciever found");

        let mut wait_queue: DelayQueue<WaitingThread> = DelayQueue::new();
        let mut async_queue = FuturesUnordered::new();
        let mut wait_keys: HashMap<XId, Vec<DelayQueueKey>> = HashMap::new();
        let mut deferred_queue: (
            UnboundedSender<DeferredThread>,
            UnboundedReceiver<DeferredThread>,
        ) = unbounded_channel();
        let mut known_deferred_threads: HashSet<XId> = HashSet::new();

        if self.is_running() || self.is_cancelled() || !self.check_lua() {
            self.done_tx.send_replace(true);
            return; // Quick exit
        }

        self.is_running.set(true);

        loop {
            if self.is_cancelled() || !self.check_lua() {
                self.is_running.set(false);
                self.done_tx.send_replace(true);
                return;
            }

            tokio::select! {
                Some(event) = rx.recv() => {
                    match event {
                        SchedulerEvent::Wait { delay_args, thread, start_at, duration } => {
                            let Some(lua) = self.lua.try_upgrade() else {
                                self.is_running.set(false);
                                self.done_tx.send_replace(true);
                                return
                            };

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
                            let _ = deferred_queue.0.send(DeferredThread {
                                thread,
                                args,
                                xid: xid.clone()
                            });
                            known_deferred_threads.insert(xid);
                        },
                        SchedulerEvent::AddAsync { thread, fut } => {
                            let thread_err = thread.clone();
                            async_queue.push(
                                std::panic::AssertUnwindSafe(
                                    fut
                                    .map(move |x| (thread, x))
                                )
                                .catch_unwind()
                                .map_err(move |e| (thread_err, e))
                            );
                        }
                        SchedulerEvent::Clear {} => {
                            wait_queue.clear();
                            wait_keys.clear();
                            self.cancel.borrow_mut().clear();
                            while let Some(_) = deferred_queue.1.recv().await {}
                        }
                        SchedulerEvent::Close {} => {
                            self.is_running.set(false);
                            self.done_tx.send_replace(true);
                            return;
                        }
                    }
                },
                Some(xid) = cancel_rx.recv() => {
                    if let Some(keys) = wait_keys.get(&xid) {
                        for key in keys {
                            wait_queue.remove(key);
                        }
                        wait_keys.remove(&xid);
                    };

                    known_deferred_threads.remove(&xid);
                    self.cancel.borrow_mut().remove(&xid);
                },
                Some(value) = wait_queue.next() => {
                    let Some(lua) = self.lua.try_upgrade() else {
                        self.is_running.set(false);
                        self.done_tx.send_replace(true);
                        return
                    };

                    let key = value.key();
                    let inner = value.into_inner();

                    let xid = XId::from_ptr(inner.thread.to_pointer());

                    if self.cancel.borrow_mut().remove(&xid) {
                        continue;
                    }

                    let r = match inner.delay_args {
                        Some(args) => inner.thread.resume(args),
                        None => inner.thread.resume(inner.start_at.elapsed().as_secs_f64())
                    };

                    self.feedback.on_response(
                        "WaitingThread",
                        &inner.thread,
                        r,
                    );

                    if wait_keys.contains_key(&xid) {
                        if let Some(old_keys) = wait_keys.remove(&xid) {
                            let mut new_keys = Vec::with_capacity(old_keys.len() - 1);
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
                Some(deferred_thread) = deferred_queue.1.recv() => {
                    let Some(lua) = self.lua.try_upgrade() else {
                        self.is_running.set(false);
                        self.done_tx.send_replace(true);
                        return
                    };

                    if self.cancel.borrow_mut().remove(&deferred_thread.xid) {
                        continue;
                    }

                    if known_deferred_threads.contains(&deferred_thread.xid) {
                        let r = deferred_thread.thread.resume(deferred_thread.args);
                        self.feedback.on_response(
                            "DeferredThread",
                            &deferred_thread.thread,
                            r,
                        );

                        known_deferred_threads.remove(&deferred_thread.xid);
                    }
                },
                Some(resp) = async_queue.next() => {
                    let Some(lua) = self.lua.try_upgrade() else {
                        self.is_running.set(false);
                        self.done_tx.send_replace(true);
                        return;
                    };

                    match resp {
                        Ok((thread, async_resp)) => {
                            match thread.status() {
                                mlua::ThreadStatus::Error | mlua::ThreadStatus::Finished => {}
                                _ => {
                                    match async_resp {
                                        Ok(resp) => {
                                            let r = thread.resume(resp);
                                            self.feedback.on_response(
                                                "DeferredThread",
                                                &thread,
                                                r,
                                            );
                                        },
                                        Err(e) => {
                                            let r = thread.resume_error::<mlua::MultiValue>(e.to_string());
                                            self.feedback.on_response(
                                                "DeferredThread",
                                                &thread,
                                                r,
                                            );
                                        }
                                    }
                                }
                            };
                        },
                        Err((thread, e)) => {
                            let mut error_payload = format!("Error in async thread: {:?}", e);
                            if let Some(error) = e.downcast_ref::<String>() {
                                error_payload = format!("Error in async thread: {}", error);
                            }
                            if let Some(error) = e.downcast_ref::<&str>() {
                                error_payload = format!("Error in async thread: {}", error);
                            }

                            let r = thread.resume_error::<mlua::MultiValue>(error_payload);
                            self.feedback.on_response(
                                "DeferredThread",
                                &thread,
                                r,
                            );
                        }
                    }
                }
            };

            if async_queue.is_empty()
                && wait_queue.is_empty()
                && deferred_queue.1.is_empty()
                && rx.is_empty()
                && cancel_rx.is_empty()
            {
                self.done_tx.send_replace(true);
            } else {
                self.done_tx.send_replace(false);
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
    pub fn push_event(&self, event: SchedulerEvent) {
        self.done_tx.send_replace(false);
        let _ = self.tx.send(event);
    }

    /// Cancels a task by its ID
    pub fn cancel_task(&self, xid: XId) {
        self.cancel.borrow_mut().insert(xid);
    }

    pub async fn wait_till_done(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut done_rx = self.done_rx.write().await;

        done_rx.wait_for(|val| *val).await?;
        Ok(())
    }
}
