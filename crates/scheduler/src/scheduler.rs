use crate::ThreadInfo;
use smol::{channel, Executor};
use std::sync::Arc;

#[derive(Debug)]
pub struct Scheduler {
    pub(crate) spawn_pool: (
        channel::Sender<(usize, ThreadInfo)>,
        channel::Receiver<(usize, ThreadInfo)>,
    ),
    pub(crate) yield_pool: (
        channel::Sender<(usize, channel::Sender<mlua::MultiValue>)>,
        channel::Receiver<(usize, channel::Sender<mlua::MultiValue>)>,
    ),
    pub(crate) result_pool: (
        channel::Sender<(usize, channel::Sender<mlua::Result<mlua::MultiValue>>)>,
        channel::Receiver<(usize, channel::Sender<mlua::Result<mlua::MultiValue>>)>,
    ),
    pub(crate) executor: Executor<'static>,

    // pub(crate) threads: Arc<Mutex<Vec<mlua::Thread>>>,
    pub errors: Vec<mlua::Error>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            spawn_pool: channel::unbounded(),
            yield_pool: channel::unbounded(),
            result_pool: channel::unbounded(),
            executor: Executor::new(),

            errors: Default::default(),
        }
    }

    pub fn setup(self, lua: &mlua::Lua) {
        lua.set_app_data(Arc::new(self));
    }
}
