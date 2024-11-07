use crate::ThreadInfo;
use smol::{channel, Executor};
use std::sync::Arc;

#[derive(Debug)]
pub struct Scheduler {
    pub(crate) pool: (
        channel::Sender<(usize, ThreadInfo)>,
        channel::Receiver<(usize, ThreadInfo)>,
    ),
    pub(crate) yield_: (
        channel::Sender<(usize, channel::Sender<mlua::MultiValue>)>,
        channel::Receiver<(usize, channel::Sender<mlua::MultiValue>)>,
    ),
    pub(crate) executor: Arc<Executor<'static>>,

    // pub(crate) threads: Arc<Mutex<Vec<mlua::Thread>>>,
    pub errors: Vec<mlua::Error>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            pool: channel::unbounded(),
            yield_: channel::unbounded(),
            executor: Arc::new(Executor::new()),

            errors: Default::default(),
        }
    }

    pub fn setup(self, lua: &mlua::Lua) {
        lua.set_app_data(Scheduler {
            pool: channel::unbounded(),
            yield_: channel::unbounded(),
            executor: Arc::new(Executor::new()),

            errors: Default::default(),
        });
    }
}
