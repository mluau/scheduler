mod r#async;
pub mod taskmgr;
pub mod userdata;
pub mod task;

pub use r#async::LuaSchedulerAsync;
pub use taskmgr::TaskManager;

pub const IS_SEND: bool = false;

pub trait MaybeSync {}
impl<T> MaybeSync for T {}

// Use XRc in case we want to add a Send feature in the future
pub type XRc<T> = std::rc::Rc<T>;

// Use XRefCell in case we want to add a Send feature in the future
pub type XRefCell<T> = std::cell::RefCell<T>;
