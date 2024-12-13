use std::collections::HashMap;

use mlua_scheduler::{taskmgr::SchedulerFeedback, XRc, XRefCell};

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
        tm: &mlua_scheduler::taskmgr::TaskManager,
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
        _tm: &mlua_scheduler::taskmgr::TaskManager,
        _th: &mlua::Thread,
        _result: Option<&Result<mlua::MultiValue, mlua::Error>>,
    ) {
    }
}

/// Tracks the results of threads
#[derive(Clone)]
pub struct ThreadResultTracker {
    results: XRc<XRefCell<HashMap<String, Result<mlua::MultiValue, mlua::Error>>>>,
}

impl Default for ThreadResultTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadResultTracker {
    /// Creates a new thread result tracker
    pub fn new() -> Self {
        Self {
            results: XRc::new(XRefCell::new(HashMap::new())),
        }
    }

    /// Adds a new result to the tracker
    pub fn add_result(&self, thread: mlua::Thread, result: Result<mlua::MultiValue, mlua::Error>) {
        self.results
            .borrow_mut()
            .insert(format!("{:?}", thread.to_pointer()), result);
    }

    /// Removes a result from the tracker
    pub fn remove_result(&self, thread: mlua::Thread) {
        self.results
            .borrow_mut()
            .remove(&format!("{:?}", thread.to_pointer()));
    }

    /// Adds a new result to the tracker
    pub fn add_result_str(&self, thread: String, result: Result<mlua::MultiValue, mlua::Error>) {
        self.results.borrow_mut().insert(thread, result);
    }

    /// Removes a result from the tracker
    pub fn remove_result_str(&self, thread: &str) {
        self.results.borrow_mut().remove(thread);
    }

    /// Gets the result of a thread
    pub fn get_result(
        &self,
        thread: &mlua::Thread,
    ) -> Option<Result<mlua::MultiValue, mlua::Error>> {
        self.results
            .borrow()
            .get(&format!("{:?}", thread.to_pointer()))
            .cloned()
    }
}

impl SchedulerFeedback for ThreadResultTracker {
    fn on_response(
        &self,
        _label: &str,
        _tm: &mlua_scheduler::taskmgr::TaskManager,
        th: &mlua::Thread,
        result: Option<&Result<mlua::MultiValue, mlua::Error>>,
    ) {
        // Replace the result if it exists
        if let Some(result) = result {
            self.add_result(th.clone(), result.clone());
        }
    }
}

/// An error tracker that saves errors about both the thread and threads it spawns
/// to a queue
pub struct ThreadErrorTracker {
    /// The thread tracker
    pub tracker: ThreadTracker,

    /// Here, mlua::Thread is the thread which error'd, Option<String> is the known metadata about the initiator, and the mlua::Error is the error
    pub error_queue:
        XRc<XRefCell<std::collections::VecDeque<(mlua::Thread, Option<String>, mlua::Error)>>>,
}

impl ThreadErrorTracker {
    /// Creates a new thread error tracker
    pub fn new(tracker: ThreadTracker) -> Self {
        Self {
            tracker,
            error_queue: XRc::new(XRefCell::new(std::collections::VecDeque::new())),
        }
    }
}

impl SchedulerFeedback for ThreadErrorTracker {
    fn on_response(
        &self,
        _label: &str,
        _tm: &mlua_scheduler::taskmgr::TaskManager,
        th: &mlua::Thread,
        result: Option<&Result<mlua::MultiValue, mlua::Error>>,
    ) {
        if let Some(Err(e)) = result {
            let initiator = self.tracker.get_initiator(th).unwrap_or_else(|| th.clone());

            // Send the error to the error queue
            self.error_queue.borrow_mut().push_back((
                initiator,
                self.tracker.get_metadata(th),
                e.clone(),
            ));
        }
    }
}

/// A scheduler feedback to nuclear kill related threads if even one errors
///
/// This may be useful but makes pcall and co. useless. Not used in Anti-Raid normally
/// unless needed for emergency purposes
pub struct ThreadTrackerKillInitiatorOnError {
    pub tracker: ThreadTracker,
    pub error_channel: flume::Sender<(Vec<mlua::Thread>, mlua::Error)>,
    reset_fn: mlua::Function,
}

impl ThreadTrackerKillInitiatorOnError {
    pub fn new(
        lua: &mlua::Lua,
        tracker: ThreadTracker,
        error_channel: flume::Sender<(Vec<mlua::Thread>, mlua::Error)>,
    ) -> mlua::Result<Self> {
        let reset_fn = lua.create_function(|_, _args: mlua::MultiValue| {
            mlua::Result::<()>::Err(mlua::Error::external("Thread was reset due to an error"))
        })?;

        Ok(Self {
            tracker,
            error_channel,
            reset_fn,
        })
    }
}

impl SchedulerFeedback for ThreadTrackerKillInitiatorOnError {
    fn on_response(
        &self,
        _label: &str,
        _tm: &mlua_scheduler::taskmgr::TaskManager,
        th: &mlua::Thread,
        result: Option<&Result<mlua::MultiValue, mlua::Error>>,
    ) {
        if let Some(Err(e)) = result {
            let related_threads = self.tracker.get_related_threads(th);

            // Send the error to the error channel
            let _ = self
                .error_channel
                .send((related_threads.clone(), e.clone()))
                .map_err(|e| log::error!("Error sending error to channel: {}", e));

            // Reset all related threads
            for th in related_threads {
                self.tracker.remove_thread(th.clone());

                if let Err(e) = th.reset(self.reset_fn.clone()) {
                    log::error!("Error resetting thread: {}", e);
                }
            }
        }
    }
}
