use crate::ThreadInfo;
use indexmap::IndexMap;
use mlua::ExternalResult;
use smol::{channel, Executor};
use std::{collections::HashMap, sync::Arc};

pub(crate) type Pool<T> = (channel::Sender<T>, channel::Receiver<T>);
pub(crate) type ThreadPool<T> = Pool<(usize, T)>;

#[derive(Debug)]
pub struct Scheduler {
    pub(crate) spawn_pool: ThreadPool<ThreadInfo>,
    pub(crate) result_pool: ThreadPool<mlua::Result<mlua::MultiValue>>,
    pub(crate) yield_pool: ThreadPool<channel::Sender<mlua::MultiValue>>,
    pub(crate) cancel_pool: Pool<usize>,
    pub(crate) result_sender_pool: ThreadPool<channel::Sender<mlua::Result<mlua::MultiValue>>>,
    pub executor: Executor<'static>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            spawn_pool: channel::unbounded(),
            result_pool: channel::unbounded(),
            yield_pool: channel::unbounded(),
            cancel_pool: channel::unbounded(),
            result_sender_pool: channel::unbounded(),
            executor: Executor::new(),
        }
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn setup(self, lua: &mlua::Lua) -> Arc<Self> {
        let arc = Arc::new(self);
        lua.set_app_data(Arc::clone(&arc));
        arc
    }

    pub async fn run(&self) -> mlua::Result<()> {
        let mut threads: IndexMap<usize, ThreadInfo> = IndexMap::new();
        let mut finished_threads: HashMap<usize, mlua::Result<mlua::MultiValue>> = HashMap::new();

        let mut thread_yield_senders: HashMap<usize, channel::Sender<mlua::MultiValue>> =
            HashMap::new();
        let mut thread_result_senders: HashMap<
            usize,
            channel::Sender<mlua::Result<mlua::MultiValue>>,
        > = HashMap::new();

        loop {
            'tick: for _ in 0..10 {
                if !self.executor.try_tick() {
                    break 'tick;
                }
            }

            while let Ok((thread_id, thread_info)) = self.spawn_pool.1.try_recv() {
                if let Some(sender) = thread_yield_senders.remove(&thread_id) {
                    sender.send(thread_info.1.clone()).await.into_lua_err()?;
                }

                threads.insert_sorted(thread_id, thread_info);
            }

            while let Ok((thread_id, sender)) = self.yield_pool.1.try_recv() {
                threads.shift_remove(&thread_id);
                thread_yield_senders.insert(thread_id, sender);
            }

            while let Ok(thread_id) = self.cancel_pool.1.try_recv() {
                threads.shift_remove(&thread_id);
            }

            for (thread_id, thread_info) in &threads {
                if let Some(result) = crate::tick_thread(thread_info) {
                    // thread finished
                    finished_threads.insert(*thread_id, result);
                };
            }

            while let Ok((thread_id, sender)) = self.result_sender_pool.1.try_recv() {
                thread_result_senders.insert(thread_id, sender);
            }

            while let Ok((thread_id, thread_result)) = self.result_pool.1.try_recv() {
                threads.shift_remove(&thread_id);
                finished_threads.insert(thread_id, thread_result);
            }

            for (thread_id, thread_result) in &finished_threads {
                if threads.contains_key(thread_id) {
                    if let Err(err) = &thread_result {
                        eprintln!("{err}");
                    }

                    threads.shift_remove(thread_id);
                }

                if let Some(sender) = thread_result_senders.remove(thread_id) {
                    sender.send(thread_result.clone()).await.into_lua_err()?;
                }
            }

            if self.executor.is_empty() & thread_yield_senders.is_empty() & threads.is_empty() {
                break;
            };
        }

        Ok(())
    }
}
