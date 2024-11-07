use mlua::ExternalResult;
use smol::{channel, Executor, Task};
use std::{future::Future, sync::Arc};

pub mod functions;

#[derive(Debug)]
struct ThreadInfo(mlua::Thread, mlua::MultiValue, usize);

#[derive(Debug)]
pub struct Scheduler {
    pub(crate) pool: (channel::Sender<ThreadInfo>, channel::Receiver<ThreadInfo>),
    // short for suspend :)
    pub(crate) sus: (channel::Sender<usize>, channel::Receiver<usize>),
    pub(crate) executor: Arc<Executor<'static>>,

    // pub(crate) threads: Arc<Mutex<Vec<mlua::Thread>>>,
    pub errors: Vec<mlua::Error>,
}

pub fn setup_scheduler(lua: &mlua::Lua) {
    lua.set_app_data(Scheduler {
        pool: channel::unbounded(),
        sus: channel::unbounded(),
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

pub async fn yield_thread(lua: &mlua::Lua, thread: mlua::Thread) -> mlua::Result<()> {
    let sus = {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();
        scheduler.sus.0.clone()
    };

    sus.send(thread.to_pointer() as usize).await.into_lua_err()
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
        pool.send(ThreadInfo(thread_inner, args_inner, thread_id))
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

async fn process_thread(thread_info: ThreadInfo) -> mlua::Result<()> {
    while let mlua::ThreadStatus::Resumable = thread_info.0.status() {
        if let Err(err) = thread_info.0.resume::<()>(thread_info.1.clone()) {
            eprintln!("{err}");
        }

        smol::future::yield_now().await;
    }

    Ok(())
}

pub async fn await_scheduler(lua: &mlua::Lua) -> mlua::Result<Scheduler> {
    let (executor, pool, sus) = {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();
        (
            Arc::clone(&scheduler.executor),
            scheduler.pool.1.clone(),
            scheduler.sus.1.clone(),
        )
    };

    loop {
        executor.try_tick();

        while let Ok(thread_info) = pool.try_recv() {
            executor.spawn(process_thread(thread_info)).detach();
        }

        while let Ok(thread_id) = sus.try_recv() {
            // TODO: tell the scheduler to not poll this thread
        }

        if executor.is_empty() {
            break;
        };
    }

    lua.remove_app_data::<Scheduler>()
        .ok_or_else(|| mlua::Error::runtime("Scheduler not found in app data container"))
}
