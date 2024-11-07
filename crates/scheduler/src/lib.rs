use mlua::ExternalResult;
use scheduler::Scheduler;
use smol::{channel, Task};
use std::{collections::HashMap, future::Future, sync::Arc};

pub mod functions;
pub mod scheduler;
pub mod traits;

#[derive(Debug)]
pub(crate) struct ThreadInfo(mlua::Thread, mlua::MultiValue);

#[derive(Debug, Clone, Copy)]
pub enum SpawnProt {
    Spawn,
    Defer,
}

fn spawn_future<T>(lua: &mlua::Lua, future: impl Future<Output = T> + Send + 'static) -> Task<T>
where
    T: Send + 'static,
{
    let executor = {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();
        Arc::clone(&scheduler.executor)
    };

    executor.spawn(future)
}

async fn yield_thread(lua: &mlua::Lua, thread: mlua::Thread) -> mlua::Result<mlua::MultiValue> {
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

fn spawn_thread<A: mlua::IntoLuaMulti>(
    lua: &mlua::Lua,
    thread: mlua::Thread,
    prot: SpawnProt,
    args: A,
) -> mlua::Result<mlua::Thread> {
    if !matches!(thread.status(), mlua::ThreadStatus::Resumable) {
        return Err(mlua::Error::CoroutineUnresumable);
    }

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

fn tick_thread(thread_info: &ThreadInfo) {
    if let mlua::ThreadStatus::Resumable = thread_info.0.status() {
        if let Err(err) = thread_info.0.resume::<()>(thread_info.1.clone()) {
            eprintln!("{err}");
        }
    }
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

        let mut finished_threads: Vec<usize> = Vec::new();

        for (thread_id, thread_info) in &threads {
            tick_thread(&thread_info);

            if let mlua::ThreadStatus::Finished = thread_info.0.status() {
                finished_threads.push(*thread_id);
            }
        }

        for thread_id in finished_threads {
            threads.remove(&thread_id);
        }

        if executor.is_empty() & suspended_threads.is_empty() {
            break;
        };
    }

    lua.remove_app_data::<Scheduler>()
        .ok_or_else(|| mlua::Error::runtime("Scheduler not found in app data container"))
}
