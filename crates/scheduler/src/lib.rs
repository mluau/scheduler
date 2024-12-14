mod r#async;
pub mod taskmgr;
pub mod userdata;

pub use r#async::LuaSchedulerAsync;
pub use taskmgr::TaskManager;

#[cfg(feature = "send")]
pub const IS_SEND: bool = true;

#[cfg(not(feature = "send"))]
pub const IS_SEND: bool = false;

#[cfg(feature = "send")]
pub trait MaybeSync: Sync {}
#[cfg(feature = "send")]
impl<T: Sync> MaybeSync for T {}
#[cfg(not(feature = "send"))]
pub trait MaybeSync {}
#[cfg(not(feature = "send"))]
impl<T> MaybeSync for T {}

/// Spawns a function on the Lua runtime
pub fn spawn_thread(lua: mlua::Lua, th: mlua::Thread, args: mlua::MultiValue) {
    let task_msg = taskmgr::get(&lua);
    task_msg.add_deferred_thread_front(th, args);
}

// Use XRc in case we want to add a Send feature in the future
#[cfg(not(feature = "send"))]
pub type XRc<T> = std::rc::Rc<T>;
#[cfg(feature = "send")]
pub type XRc<T> = std::sync::Arc<T>;

// Use XRefCell in case we want to add a Send feature in the future
#[cfg(not(feature = "send"))]
pub type XRefCell<T> = std::cell::RefCell<T>;

#[cfg(feature = "send")]
pub struct XRefCell<T> {
    inner: std::sync::RwLock<T>,
}

#[cfg(feature = "send")]
impl<T> XRefCell<T> {
    pub fn new(inner: T) -> Self {
        Self {
            inner: std::sync::RwLock::new(inner),
        }
    }

    pub fn borrow(&self) -> std::sync::RwLockReadGuard<T> {
        self.inner.read().unwrap()
    }

    pub fn borrow_mut(&self) -> std::sync::RwLockWriteGuard<T> {
        self.inner.write().unwrap()
    }
}
