use std::collections::HashMap;

use mlua_scheduler::{taskmgr::SchedulerFeedback, TaskManager, XRc, XRefCell};

/// A multiple scheduler feedback that can be used to combine multiple scheduler feedbacks
pub struct MultipleSchedulerFeedback {
    feedbacks: Vec<Box<dyn SchedulerFeedback>>,
}

impl MultipleSchedulerFeedback {
    /// Creates a new multiple scheduler feedback
    pub fn new(feedbacks: Vec<Box<dyn SchedulerFeedback>>) -> Self {
        Self { feedbacks }
    }

    /// Adds a new feedback to the multiple scheduler feedback
    pub fn add_feedback(&mut self, feedback: Box<dyn SchedulerFeedback>) {
        self.feedbacks.push(feedback);
    }
}

impl SchedulerFeedback for MultipleSchedulerFeedback {
    fn on_thread_add(
        &self,
        label: &str,
        creator: &mlua::Thread,
        thread: &mlua::Thread,
    ) -> mlua::Result<()> {
        for feedback in &self.feedbacks {
            feedback.on_thread_add(label, creator, thread)?;
        }

        Ok(())
    }

    fn on_response(
        &self,
        label: &str,
        tm: &TaskManager,
        th: &mlua::Thread,
        result: Option<&Result<mlua::MultiValue, mlua::Error>>,
    ) {
        for feedback in &self.feedbacks {
            feedback.on_response(label, tm, th, result);
        }
    }
}

/// Tracks the threads known to the scheduler to the thread which initiated them
#[derive(Clone)]
pub struct ThreadTracker {
    threads_known: XRc<XRefCell<HashMap<String, String>>>,
    thread_hashmap: XRc<XRefCell<HashMap<String, mlua::Thread>>>,
    thread_metadata: XRc<XRefCell<HashMap<String, String>>>,
}

impl Default for ThreadTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadTracker {
    /// Creates a new thread tracker
    pub fn new() -> Self {
        Self {
            threads_known: XRc::new(XRefCell::new(HashMap::new())),
            thread_hashmap: XRc::new(XRefCell::new(HashMap::new())),
            thread_metadata: XRc::new(XRefCell::new(HashMap::new())),
        }
    }

    /// Sets metadata for a thread. Useful for storing information about a thread
    pub fn set_metadata(&self, thread: mlua::Thread, metadata: String) {
        self.thread_metadata
            .borrow_mut()
            .insert(format!("{:?}", thread.to_pointer()), metadata);
    }

    /// Gets metadata for a thread
    pub fn get_metadata(&self, thread: &mlua::Thread) -> Option<String> {
        self.thread_metadata
            .borrow()
            .get(&format!("{:?}", thread.to_pointer()))
            .cloned()
    }

    /// Given metadata, returns the first thread that has that metadata
    pub fn get_thread_from_metadata(&self, metadata: &str) -> Option<mlua::Thread> {
        for (key, value) in self.thread_metadata.borrow().iter() {
            if value == metadata {
                return self.thread_hashmap.borrow().get(key).cloned();
            }
        }

        None
    }

    /// Adds a new thread to the tracker
    pub fn add_thread(&self, thread: mlua::Thread, initiator: mlua::Thread) {
        self.threads_known.borrow_mut().insert(
            format!("{:?}", thread.to_pointer()),
            format!("{:?}", initiator.to_pointer()),
        );

        self.thread_hashmap
            .borrow_mut()
            .insert(format!("{:?}", thread.to_pointer()), thread);

        self.thread_hashmap
            .borrow_mut()
            .insert(format!("{:?}", initiator.to_pointer()), initiator);
    }

    /// Removes a thread from the tracker
    pub fn remove_thread(&self, thread: mlua::Thread) {
        self.threads_known
            .borrow_mut()
            .remove(&format!("{:?}", thread.to_pointer()));
    }

    /// Gets the initiator of a thread
    pub fn get_initiator(&self, thread: &mlua::Thread) -> Option<mlua::Thread> {
        let thread_ptr = self
            .threads_known
            .borrow()
            .get(&format!("{:?}", thread.to_pointer()))
            .cloned()?;

        self.thread_hashmap.borrow().get(&thread_ptr).cloned()
    }

    /// Returns a list of all related thread entries.
    ///
    /// A related thread entry whose thread or its initiator one that is either present in key or value of the hashmap.
    pub fn get_related_threads(&self, thread: &mlua::Thread) -> Vec<mlua::Thread> {
        let thread_ptr = format!("{:?}", thread.to_pointer());

        let mut related_threads = Vec::new();

        for (key, value) in self.threads_known.borrow().iter() {
            if key == &thread_ptr {
                if let Some(thread) = self.thread_hashmap.borrow().get(value) {
                    related_threads.push(thread.clone());
                }
            } else if value == &thread_ptr {
                if let Some(thread) = self.thread_hashmap.borrow().get(key) {
                    related_threads.push(thread.clone());
                }
            }
        }

        // Check for initiator
        if let Some(initiator) = self.get_initiator(thread) {
            let initiator_ptr = format!("{:?}", initiator.to_pointer());

            for (key, value) in self.threads_known.borrow().iter() {
                if key == &initiator_ptr {
                    if let Some(thread) = self.thread_hashmap.borrow().get(value) {
                        related_threads.push(thread.clone());
                    }
                } else if value == &initiator_ptr {
                    if let Some(thread) = self.thread_hashmap.borrow().get(key) {
                        related_threads.push(thread.clone());
                    }
                }
            }
        }

        related_threads
    }
}

impl SchedulerFeedback for ThreadTracker {
    fn on_thread_add(
        &self,
        _label: &str,
        creator: &mlua::Thread,
        thread: &mlua::Thread,
    ) -> mlua::Result<()> {
        // If we have the creator's initiator, then the threads initiator is the same
        // Otherwise, the threads initiator is the creator

        let pot_initiator = self.get_initiator(creator);

        if let Some(initiator) = pot_initiator {
            self.add_thread(thread.clone(), initiator);
        } else {
            self.add_thread(thread.clone(), creator.clone());
        }

        Ok(())
    }

    fn on_response(
        &self,
        _label: &str,
        _tm: &TaskManager,
        _th: &mlua::Thread,
        _result: Option<&Result<mlua::MultiValue, mlua::Error>>,
    ) {
    }
}

/// Tracks thread results and uses a channel to send them back out
#[derive(Clone)]
pub struct ThreadResultTracker {
    #[allow(clippy::type_complexity)]
    pub returns: XRc<
        XRefCell<
            HashMap<
                String,
                tokio::sync::mpsc::UnboundedSender<Option<mlua::Result<mlua::MultiValue>>>,
            >,
        >,
    >,
}

impl Default for ThreadResultTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadResultTracker {
    /// Creates a new thread tracker
    pub fn new() -> Self {
        Self {
            returns: XRc::new(XRefCell::new(HashMap::new())),
        }
    }

    fn thread_string(&self, th: &mlua::Thread) -> String {
        format!("{:?}", th.to_pointer())
    }

    /// Track a threads result
    pub fn track_thread(
        &self,
        th: &mlua::Thread,
    ) -> tokio::sync::mpsc::UnboundedReceiver<Option<mlua::Result<mlua::MultiValue>>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.returns.borrow_mut().insert(self.thread_string(th), tx);

        rx
    }
}

impl SchedulerFeedback for ThreadResultTracker {
    fn on_response(
        &self,
        _label: &str,
        _tm: &TaskManager,
        th: &mlua::Thread,
        result: Option<&mlua::Result<mlua::MultiValue>>,
    ) {
        if let Some(tx) = self.returns.borrow_mut().get(&self.thread_string(th)) {
            let _ = tx.send(match result {
                Some(Ok(mv)) => Some(Ok(mv.clone())),
                Some(Err(e)) => Some(Err(e.clone())),
                None => None,
            });
        }
    }
}
