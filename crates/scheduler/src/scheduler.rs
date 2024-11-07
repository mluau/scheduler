use crate::ThreadInfo;
use mlua::ExternalResult;
use smol::{channel, Executor};
use std::{collections::HashMap, sync::Arc};

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
    pub executor: Executor<'static>,

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

    pub fn setup(self, lua: &mlua::Lua) -> Arc<Self> {
        let arc = Arc::new(self);
        lua.set_app_data(Arc::clone(&arc));
        arc
    }

    pub async fn run(&self) -> mlua::Result<()> {
        let mut threads: HashMap<usize, ThreadInfo> = HashMap::new();
        let mut thread_yield_senders: HashMap<usize, channel::Sender<mlua::MultiValue>> =
            HashMap::new();
        let mut thread_result_senders: HashMap<
            usize,
            channel::Sender<mlua::Result<mlua::MultiValue>>,
        > = HashMap::new();

        loop {
            self.executor.try_tick();

            while let Ok((thread_id, thread_info)) = self.spawn_pool.1.try_recv() {
                if let Some(sender) = thread_yield_senders.remove(&thread_id) {
                    sender.send(thread_info.1.clone()).await.into_lua_err()?;
                }

                threads.insert(thread_id, thread_info);
            }

            while let Ok((thread_id, sender)) = self.yield_pool.1.try_recv() {
                threads.remove(&thread_id);
                thread_yield_senders.insert(thread_id, sender);
            }

            let mut finished_threads: HashMap<usize, mlua::Result<mlua::MultiValue>> =
                HashMap::new();

            for (thread_id, thread_info) in &threads {
                if let Some(result) = crate::tick_thread(&thread_info) {
                    // thread finished
                    finished_threads.insert(*thread_id, result);
                };
            }

            while let Ok((thread_id, sender)) = self.result_pool.1.try_recv() {
                thread_result_senders.insert(thread_id, sender);
            }

            for (thread_id, thread_result) in finished_threads {
                if let Err(err) = &thread_result {
                    eprintln!("{err}");
                }

                if let Some(sender) = thread_result_senders.remove(&thread_id) {
                    sender.send(thread_result).await.into_lua_err()?;
                }

                threads.remove(&thread_id);
            }

            if self.executor.is_empty() & thread_yield_senders.is_empty() & threads.is_empty() {
                break;
            };
        }

        Ok(())
    }
}
