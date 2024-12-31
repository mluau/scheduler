use std::collections::HashMap;

use mlua_scheduler::{taskmgr::SchedulerFeedback, TaskManager, XRc, XRefCell};

/// Tracks the threads known to the scheduler to the thread which initiated them
#[derive(Clone)]
pub struct ThreadTracker {
    threads_known: XRc<XRefCell<HashMap<*const std::ffi::c_void, *const std::ffi::c_void>>>,
    thread_hashmap: XRc<XRefCell<HashMap<*const std::ffi::c_void, mlua::Thread>>>,
    thread_metadata: XRc<XRefCell<HashMap<*const std::ffi::c_void, String>>>,
    #[allow(clippy::type_complexity)]
    pub returns: XRc<
        XRefCell<
            HashMap<
                *const std::ffi::c_void,
                tokio::sync::mpsc::UnboundedSender<Option<mlua::Result<mlua::MultiValue>>>,
            >,
        >,
    >,
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
            returns: XRc::new(XRefCell::new(HashMap::new())),
        }
    }

    /// Sets metadata for a thread. Useful for storing information about a thread
    pub fn set_metadata(&self, thread: mlua::Thread, metadata: String) {
        self.thread_metadata
            .borrow_mut()
            .insert(thread.to_pointer(), metadata);
    }

    /// Gets metadata for a thread
    pub fn get_metadata(&self, thread: &mlua::Thread) -> Option<String> {
        self.thread_metadata
            .borrow()
            .get(&thread.to_pointer())
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
        let ptr = thread.to_pointer();
        let initiator_ptr = initiator.to_pointer();
        self.threads_known.borrow_mut().insert(ptr, initiator_ptr);

        self.thread_hashmap.borrow_mut().insert(ptr, thread);

        self.thread_hashmap
            .borrow_mut()
            .insert(initiator_ptr, initiator);
    }

    /// Removes a thread from the tracker
    pub fn remove_thread(&self, thread: mlua::Thread) {
        self.threads_known.borrow_mut().remove(&thread.to_pointer());
    }

    /// Gets the initiator of a thread
    pub fn get_initiator(&self, thread: &mlua::Thread) -> Option<mlua::Thread> {
        let thread_ptr = self
            .threads_known
            .borrow()
            .get(&thread.to_pointer())
            .cloned()?;

        self.thread_hashmap.borrow().get(&thread_ptr).cloned()
    }

    /// Returns a list of all related thread entries.
    ///
    /// A related thread entry whose thread or its initiator one that is either present in key or value of the hashmap.
    pub fn get_related_threads(&self, thread: &mlua::Thread) -> Vec<mlua::Thread> {
        let thread_ptr = thread.to_pointer();

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
            let initiator_ptr = initiator.to_pointer();

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

    /// Track a threads result
    pub fn track_thread(
        &self,
        th: &mlua::Thread,
    ) -> tokio::sync::mpsc::UnboundedReceiver<Option<mlua::Result<mlua::MultiValue>>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.returns.borrow_mut().insert(th.to_pointer(), tx);

        rx
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
        th: &mlua::Thread,
        result: Option<Result<mlua::MultiValue, mlua::Error>>,
    ) {
        if let Some(tx) = self.returns.borrow_mut().get(&th.to_pointer()) {
            let _ = tx.send(match result {
                Some(Ok(mv)) => Some(Ok(mv)),
                Some(Err(e)) => Some(Err(e)),
                None => None,
            });
        }
    }
}
