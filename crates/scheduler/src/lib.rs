mod r#async;
pub mod taskmgr;
pub mod userdata;

#[cfg(feature = "v2_taskmgr")]
pub mod taskmgr_v2;
#[cfg(not(feature = "v2_taskmgr"))]
pub mod taskmgr_v1;

pub use r#async::LuaSchedulerAsync;
pub use taskmgr::TaskManager;

pub const IS_SEND: bool = false;

// Use XRc in case we want to add a Send feature in the future
#[cfg(feature = "send")]
pub type XRc<T> = std::sync::Arc<T>;
#[cfg(not(feature = "send"))]
pub type XRc<T> = std::rc::Rc<T>;

// Use XRefCell in case we want to add a Send feature in the future
#[cfg(not(feature = "send"))]
pub type XRefCell<T> = std::cell::RefCell<T>;
#[cfg(feature = "send")]
pub struct XRefCell<T>(std::sync::RwLock<T>);
#[cfg(feature = "send")]
impl<T> XRefCell<T> {
    pub fn new(x: T) -> Self {
        Self(std::sync::RwLock::new(x))
    }

    pub fn borrow(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.0.read().unwrap()
    }

    pub fn borrow_mut(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.0.write().unwrap()
    }
}

/// A trait that adds `Send` requirement if `send` feature is enabled.
#[cfg(feature = "send")]
pub trait MaybeSend: Send {}
#[cfg(feature = "send")]
impl<T: Send> MaybeSend for T {}

#[cfg(not(feature = "send"))]
pub trait MaybeSend {}
#[cfg(not(feature = "send"))]
impl<T> MaybeSend for T {}

/// A trait that adds `Sync` requirement if `send` feature is enabled.
#[cfg(feature = "send")]
pub trait MaybeSync: Sync {}
#[cfg(feature = "send")]
impl<T: Sync> MaybeSync for T {}

#[cfg(not(feature = "send"))]
pub trait MaybeSync {}
#[cfg(not(feature = "send"))]
impl<T> MaybeSync for T {}

/// Use Cell<bool> for non-send an AtomicBool for send
#[cfg(feature = "send")]
pub struct XBool(std::sync::atomic::AtomicBool);
#[cfg(feature = "send")]
impl XBool {
    pub fn new(i: bool) -> Self {
        Self(std::sync::atomic::AtomicBool::new(i))
    }

    pub fn get(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn set(&self, v: bool) {
        self.0.store(v, std::sync::atomic::Ordering::Release)
    }
}

#[cfg(not(feature = "send"))]
pub struct XBool(std::cell::Cell<bool>);
#[cfg(not(feature = "send"))]
impl XBool {
    pub fn new(i: bool) -> Self {
        Self(std::cell::Cell::new(i))
    }

    pub fn get(&self) -> bool {
        self.0.get()
    }

    pub fn set(&self, v: bool) {
        self.0.set(v)
    }
}

/// Use Cell<usize> for non-send an AtomicUsize for send
#[cfg(feature = "send")]
pub struct XUsize(std::sync::atomic::AtomicUsize);
#[cfg(feature = "send")]
impl XUsize {
    pub fn new(i: usize) -> Self {
        Self(std::sync::atomic::AtomicUsize::new(i))
    }

    pub fn get(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn set(&self, v: usize) {
        self.0.store(v, std::sync::atomic::Ordering::Release)
    }
}

#[cfg(not(feature = "send"))]
pub struct XUsize(std::cell::Cell<usize>);
#[cfg(not(feature = "send"))]
impl XUsize {
    pub fn new(i: usize) -> Self {
        Self(std::cell::Cell::new(i))
    }

    pub fn get(&self) -> usize {
        self.0.get()
    }

    pub fn set(&self, v: usize) {
        self.0.set(v)
    }
}

/// Either a ``*const std::ffi::c_void`` or a String depending on send/sync status
#[derive(PartialEq, Hash, Eq, Clone)]
pub struct XId(*const std::ffi::c_void);
impl XId {
    pub fn from_ptr(ptr: *const std::ffi::c_void) -> Self {
        XId(ptr)
    }
}

// SAFETY: XId is only used for hashing and equality and so is Send/Sync
unsafe impl Send for XId {}
unsafe impl Sync for XId {}