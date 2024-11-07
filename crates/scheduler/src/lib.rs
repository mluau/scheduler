use mlua::ExternalResult;
use smol::{channel, Executor, Task};
use std::{collections::HashMap, future::Future, sync::Arc};

pub mod functions;

#[derive(Debug)]
struct ThreadInfo(mlua::Thread, mlua::MultiValue);

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

pub fn setup_scheduler(lua: &mlua::Lua) {
    lua.set_app_data(Scheduler {
        pool: channel::unbounded(),
        yield_: channel::unbounded(),
        executor: Arc::new(Executor::new()),

        errors: Default::default(),
    });
}

#[derive(Debug, Clone, Copy)]
pub enum SpawnProt {
    Spawn,
    Defer,
}

pub fn spawn_future<T>(lua: &mlua::Lua, future: impl Future<Output = T> + Send + 'static) -> Task<T>
where
    T: Send + 'static,
{
    let executor = {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();
        Arc::clone(&scheduler.executor)
    };

    executor.spawn(future)
}

pub async fn yield_thread(lua: &mlua::Lua, thread: mlua::Thread) -> mlua::Result<mlua::MultiValue> {
    let yield_ = {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();
        scheduler.yield_.0.clone()
    };

    let (sender, receiver) = channel::bounded(1);

    yield_
        .send((thread.to_pointer() as usize, sender))
        .await
        .into_lua_err()?;

    receiver.recv().await.into_lua_err()
}

pub fn spawn_thread<A: mlua::IntoLuaMulti>(
    lua: &mlua::Lua,
    thread: mlua::Thread,
    prot: SpawnProt,
    args: A,
) -> mlua::Result<mlua::Thread> {
    let args = args.into_lua_multi(lua)?;

    let pool = {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();
        scheduler.pool.0.clone()
    };

    let thread_inner = thread.clone();
    let args_inner = args.clone();
    let thread_id = thread_inner.to_pointer() as _;

    spawn_future(&lua.clone(), async move {
        pool.send((thread_id, ThreadInfo(thread_inner, args_inner)))
            .await
            .expect("Failed to send thread to scheduler")
    })
    .detach();

    if matches!(prot, SpawnProt::Spawn) {
        // poll immediately
        thread.resume::<()>(args)?;
    }

    Ok(thread)
}

async fn tick_thread(thread_info: &ThreadInfo) -> mlua::Result<()> {
    if let mlua::ThreadStatus::Resumable = thread_info.0.status() {
        if let Err(err) = thread_info.0.resume::<()>(thread_info.1.clone()) {
            eprintln!("{err}");
        }

        smol::future::yield_now().await;
    }

    Ok(())
}

pub async fn await_scheduler(lua: &mlua::Lua) -> mlua::Result<Scheduler> {
    let (executor, pool, yield_) = {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();
        (
            Arc::clone(&scheduler.executor),
            scheduler.pool.1.clone(),
            scheduler.yield_.1.clone(),
        )
    };

    let mut threads: HashMap<usize, ThreadInfo> = HashMap::new();
    let mut suspended_threads: HashMap<usize, channel::Sender<mlua::MultiValue>> = HashMap::new();

    loop {
        executor.try_tick();

        while let Ok((thread_id, thread_info)) = pool.try_recv() {
            if let Some(sender) = suspended_threads.remove(&thread_id) {
                sender.send(thread_info.1.clone()).await.into_lua_err()?;
            }

            threads.insert(thread_id, thread_info);
        }

        while let Ok((thread_id, sender)) = yield_.try_recv() {
            threads.remove(&thread_id);
            suspended_threads.insert(thread_id, sender);
        }

        for thread in threads.values() {
            tick_thread(thread).await?;
        }

        if executor.is_empty() & suspended_threads.is_empty() {
            break;
        };
    }

    lua.remove_app_data::<Scheduler>()
        .ok_or_else(|| mlua::Error::runtime("Scheduler not found in app data container"))
}
