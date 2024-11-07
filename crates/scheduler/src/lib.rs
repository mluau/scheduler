use smol::{channel, Executor, Task};
use std::{future::Future, sync::Arc};

pub mod functions;

#[derive(Debug)]
struct ThreadInfo(mlua::RegistryKey, mlua::MultiValue);

#[derive(Debug)]
pub struct Scheduler {
    pub(crate) pool: (channel::Sender<ThreadInfo>, channel::Receiver<ThreadInfo>),
    pub(crate) executor: Arc<Executor<'static>>,

    // pub(crate) threads: Arc<Mutex<Vec<mlua::Thread>>>,
    pub errors: Vec<mlua::Error>,
}

pub fn setup_scheduler(lua: &mlua::Lua) {
    lua.set_app_data(Scheduler {
        pool: channel::unbounded(),
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
    let lua_inner = lua.clone();

    spawn_future(&lua.clone(), async move {
        pool.send(ThreadInfo(
            lua_inner
                .create_registry_value(thread_inner)
                .expect("Failed to send thread to registry"),
            args_inner,
        ))
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

async fn process_thread(lua: &mlua::Lua, thread_info: ThreadInfo) -> mlua::Result<()> {
    let thread: mlua::Thread = lua.registry_value(&thread_info.0)?;
    lua.remove_registry_value(thread_info.0)?;

    if let mlua::ThreadStatus::Resumable = thread.status() {
        if let Err(err) = thread.into_async::<()>(thread_info.1).await {
            eprintln!("{err}");
        }
    };

    Ok(())
}

pub async fn await_scheduler(lua: &mlua::Lua) -> mlua::Result<Scheduler> {
    let (executor, pool) = {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();
        (Arc::clone(&scheduler.executor), scheduler.pool.1.clone())
    };

    loop {
        executor.try_tick();

        while let Ok(thread_info) = pool.try_recv() {
            let lua_inner = lua.clone();

            executor
                .spawn(async move {
                    process_thread(&lua_inner, thread_info)
                        .await
                        .expect("Failed to process thread");
                })
                .detach();
        }

        // smol::future::yield_now().await;

        if executor.is_empty() {
            break;
        };
    }

    lua.remove_app_data::<Scheduler>()
        .ok_or_else(|| mlua::Error::runtime("Scheduler not found in app data container"))
}
