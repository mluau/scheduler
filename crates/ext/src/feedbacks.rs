use std::collections::HashMap;

use mlua_scheduler::{taskmgr::SchedulerFeedback, XRefCell};

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
pub struct ThreadTracker {
    threads_known: XRefCell<HashMap<String, String>>,
}

impl ThreadTracker {
    /// Creates a new thread tracker
    pub fn new() -> Self {
        Self {
            threads_known: XRefCell::new(HashMap::new()),
        }
    }

    /// Adds a new thread to the tracker
    pub fn add_thread(&self, thread: mlua::Thread, initiator: mlua::Thread) {
        self.threads_known.borrow_mut().insert(
            format!("{:?}", thread.to_pointer()),
            format!("{:?}", initiator.to_pointer()),
        );
    }

    /// Removes a thread from the tracker
    pub fn remove_thread(&self, thread: mlua::Thread) {
        self.threads_known
            .borrow_mut()
            .remove(&format!("{:?}", thread.to_pointer()));
    }

    /// Adds a new thread to the tracker
    pub fn add_thread_str(&self, thread: String, initiator: String) {
        self.threads_known.borrow_mut().insert(thread, initiator);
    }

    /// Removes a thread from the tracker
    pub fn remove_thread_str(&self, thread: &str) {
        self.threads_known.borrow_mut().remove(thread);
    }

    /// Gets the initiator of a thread
    pub fn get_initiator(&self, thread: &mlua::Thread) -> Option<String> {
        self.threads_known
            .borrow()
            .get(&format!("{:?}", thread.to_pointer()))
            .cloned()
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
            self.add_thread_str(format!("{:?}", thread.to_pointer()), initiator);
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
pub struct ThreadResultTracker {
    results: XRefCell<HashMap<String, Result<mlua::MultiValue, mlua::Error>>>,
}

impl ThreadResultTracker {
    /// Creates a new thread result tracker
    pub fn new() -> Self {
        Self {
            results: XRefCell::new(HashMap::new()),
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
    fn on_thread_add(
        &self,
        _label: &str,
        _creator: &mlua::Thread,
        _thread: &mlua::Thread,
    ) -> mlua::Result<()> {
        Ok(())
    }

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
