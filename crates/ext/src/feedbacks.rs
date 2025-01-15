use std::collections::HashMap;

use mlua_scheduler::{taskmgr::SchedulerFeedback, TaskManager, XRc, XRefCell};

/// Chain 2 feedbacks together
pub struct ChainFeedback<T: SchedulerFeedback, U: SchedulerFeedback>(pub T, pub U);

impl<T: SchedulerFeedback, U: SchedulerFeedback> ChainFeedback<T, U> {
    /// Creates a new chain feedback
    pub fn new(t: T, u: U) -> Self {
        Self(t, u)
    }

    /// Creates a new chain feedback by chaining with another feedback
    pub fn chain<V: SchedulerFeedback>(self, v: V) -> ChainFeedback<Self, V> {
        ChainFeedback(self, v)
    }
}

impl<T: SchedulerFeedback, U: SchedulerFeedback> SchedulerFeedback for ChainFeedback<T, U> {
    fn on_thread_add(
        &self,
        label: &str,
        creator: &mlua::Thread,
        thread: &mlua::Thread,
    ) -> mlua::Result<()> {
        self.0.on_thread_add(label, creator, thread)?;
        self.1.on_thread_add(label, creator, thread)
    }

    fn on_response(
        &self,
        label: &str,
        tm: &TaskManager,
        th: &mlua::Thread,
        result: Option<Result<mlua::MultiValue, mlua::Error>>,
    ) {
        self.0.on_response(label, tm, th, result.clone());
        self.1.on_response(label, tm, th, result);
    }
}

/// Not all scheduler feedbacks need both on_thread_add and on_response
///
/// Some only need on_thread_add. As such, using a ThreadAddMiddleware+ThreadAddMiddlewareFeedback
/// can be more efficient
#[cfg(not(feature = "multithread"))]
pub trait ThreadAddMiddleware {
    fn on_thread_add(
        &self,
        label: &str,
        creator: &mlua::Thread,
        thread: &mlua::Thread,
    ) -> mlua::Result<()>;
}

/// Not all scheduler feedbacks need both on_thread_add and on_response
///
/// Some only need on_thread_add. As such, using a ThreadAddMiddleware+ThreadAddMiddlewareFeedback
/// can be more efficient
#[cfg(feature = "multithread")]
pub trait ThreadAddMiddleware: Send + Sync {
    fn on_thread_add(
        &self,
        label: &str,
        creator: &mlua::Thread,
        thread: &mlua::Thread,
    ) -> mlua::Result<()>;
}

/// Attaches a ThreadAddMiddleware to a SchedulerFeedback
pub struct ThreadAddMiddlewareFeedback<T: SchedulerFeedback, U: ThreadAddMiddleware>(pub T, pub U);

impl<T: SchedulerFeedback, U: ThreadAddMiddleware> ThreadAddMiddlewareFeedback<T, U> {
    /// Creates a new ThreadAddMiddlewareFeedback
    pub fn new(t: T, u: U) -> Self {
        Self(t, u)
    }
}

impl<T: SchedulerFeedback, U: ThreadAddMiddleware> SchedulerFeedback
    for ThreadAddMiddlewareFeedback<T, U>
{
    fn on_thread_add(
        &self,
        label: &str,
        creator: &mlua::Thread,
        thread: &mlua::Thread,
    ) -> mlua::Result<()> {
        self.0.on_thread_add(label, creator, thread)?;
        self.1.on_thread_add(label, creator, thread)
    }

    fn on_response(
        &self,
        label: &str,
        tm: &TaskManager,
        th: &mlua::Thread,
        result: Option<Result<mlua::MultiValue, mlua::Error>>,
    ) {
        self.0.on_response(label, tm, th, result);
    }
}

#[cfg(not(feature = "multithread"))]
#[derive(Hash, Eq, PartialEq)]
pub struct ThreadPtr(*const std::ffi::c_void);

#[cfg(feature = "multithread")]
#[derive(Hash, Eq, PartialEq)]
pub struct ThreadPtr(String);

#[cfg(not(feature = "multithread"))]
impl ThreadPtr {
    pub fn new(thread: &mlua::Thread) -> Self {
        Self(thread.to_pointer())
    }
}

#[cfg(feature = "multithread")]
impl ThreadPtr {
    pub fn new(thread: &mlua::Thread) -> Self {
        Self(format!("{:?}", thread.to_pointer()))
    }
}

/// Tracks the threads known to the scheduler to the thread which initiated them
#[derive(Clone)]
pub struct ThreadTracker {
    #[allow(clippy::type_complexity)]
    pub returns: XRc<
        XRefCell<
            HashMap<
                ThreadPtr,
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
            returns: XRc::new(XRefCell::new(HashMap::new())),
        }
    }

    /// Track a threads result
    pub fn track_thread(
        &self,
        th: &mlua::Thread,
    ) -> tokio::sync::mpsc::UnboundedReceiver<Option<mlua::Result<mlua::MultiValue>>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.returns.borrow_mut().insert(ThreadPtr::new(th), tx);

        rx
    }
}

impl SchedulerFeedback for ThreadTracker {
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
        _tm: &TaskManager,
        th: &mlua::Thread,
        result: Option<Result<mlua::MultiValue, mlua::Error>>,
    ) {
        if let Some(tx) = self.returns.borrow_mut().get(&ThreadPtr::new(th)) {
            let _ = tx.send(result);
        }
    }
}

/// A thread limit middleware
pub struct ThreadLimitMiddleware {
    limit: XRefCell<usize>,
    created: XRefCell<usize>,
}

impl ThreadLimitMiddleware {
    /// Creates a new thread limit middleware
    pub fn new(limit: usize) -> Self {
        Self {
            limit: XRefCell::new(limit),
            created: XRefCell::new(0),
        }
    }

    /// Sets the thread limit
    ///
    /// Panics if called concurrently
    pub fn set_limit(&self, limit: usize) {
        let mut limit_g = self.limit.borrow_mut();
        *limit_g = limit;
    }
}

impl ThreadAddMiddleware for ThreadLimitMiddleware {
    fn on_thread_add(
        &self,
        _label: &str,
        _creator: &mlua::Thread,
        _thread: &mlua::Thread,
    ) -> mlua::Result<()> {
        let limit = *self.limit.borrow();
        let mut created = self.created.borrow_mut();
        *created += 1;

        if *created > limit {
            return Err(mlua::Error::RuntimeError(
                "Thread limit reached".to_string(),
            ));
        }

        Ok(())
    }
}
