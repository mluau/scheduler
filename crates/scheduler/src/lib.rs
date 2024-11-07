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
    let scheduler = Arc::clone(&lua.app_data_ref::<Arc<Scheduler>>().unwrap());

    scheduler.executor.spawn(future)
}

async fn yield_thread(lua: &mlua::Lua, thread: mlua::Thread) -> mlua::Result<mlua::MultiValue> {
    let scheduler = Arc::clone(&lua.app_data_ref::<Arc<Scheduler>>().unwrap());

    let (sender, receiver) = channel::bounded(1);

    scheduler
        .yield_pool
        .0
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

    let scheduler = Arc::clone(&lua.app_data_ref::<Arc<Scheduler>>().unwrap());

    let thread_inner = thread.clone();
    let args_inner = args.clone();
    let thread_id = thread_inner.to_pointer() as _;

    spawn_future(&lua.clone(), async move {
        scheduler
            .spawn_pool
            .0
            .send((thread_id, ThreadInfo(thread_inner, args_inner)))
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

async fn await_thread(lua: &mlua::Lua, thread: mlua::Thread) -> mlua::Result<mlua::MultiValue> {
    let scheduler = Arc::clone(&lua.app_data_ref::<Arc<Scheduler>>().unwrap());

    let thread_id = thread.to_pointer() as _;
    let (sender, receiver) = channel::bounded(1);

    spawn_future(&lua.clone(), async move {
        scheduler
            .result_pool
            .0
            .send((thread_id, sender))
            .await
            .expect("Failed to send thread to scheduler")
    })
    .detach();

    receiver.recv().await.into_lua_err()?
}

fn tick_thread(thread_info: &ThreadInfo) -> Option<mlua::Result<mlua::MultiValue>> {
    if let mlua::ThreadStatus::Resumable = thread_info.0.status() {
        let result = thread_info
            .0
            .resume::<mlua::MultiValue>(thread_info.1.clone());

        match &result {
            Ok(value) => {
                if value.get(0).is_some_and(|value| {
                    value
                        .as_light_userdata()
                        .is_some_and(|l| l == mlua::Lua::poll_pending())
                }) {
                    None
                } else {
                    Some(result)
                }
            }
            Err(err) => Some(Err(err.to_owned())),
        }
    } else {
        None
    }
}

pub async fn await_scheduler(lua: &mlua::Lua) -> mlua::Result<Arc<Scheduler>> {
    let scheduler = Arc::clone(&lua.app_data_ref::<Arc<Scheduler>>().unwrap());

    let mut threads: HashMap<usize, ThreadInfo> = HashMap::new();
    let mut thread_yield_senders: HashMap<usize, channel::Sender<mlua::MultiValue>> =
        HashMap::new();
    let mut thread_result_senders: HashMap<usize, channel::Sender<mlua::Result<mlua::MultiValue>>> =
        HashMap::new();

    loop {
        scheduler.executor.try_tick();

        while let Ok((thread_id, thread_info)) = scheduler.spawn_pool.1.try_recv() {
            if let Some(sender) = thread_yield_senders.remove(&thread_id) {
                sender.send(thread_info.1.clone()).await.into_lua_err()?;
            }

            threads.insert(thread_id, thread_info);
        }

        while let Ok((thread_id, sender)) = scheduler.yield_pool.1.try_recv() {
            threads.remove(&thread_id);
            thread_yield_senders.insert(thread_id, sender);
        }

        let mut finished_threads: HashMap<usize, mlua::Result<mlua::MultiValue>> = HashMap::new();

        for (thread_id, thread_info) in &threads {
            if let Some(result) = tick_thread(&thread_info) {
                // thread finished
                finished_threads.insert(*thread_id, result);
            };
        }

        while let Ok((thread_id, sender)) = scheduler.result_pool.1.try_recv() {
            thread_result_senders.insert(thread_id, sender);
        }

        for (thread_id, thread_result) in finished_threads {
            if let Some(sender) = thread_result_senders.remove(&thread_id) {
                sender.send(thread_result).await.into_lua_err()?;
            }

            threads.remove(&thread_id);
        }

        if scheduler.executor.is_empty() & thread_yield_senders.is_empty() {
            break;
        };
    }

    lua.remove_app_data::<Arc<Scheduler>>()
        .ok_or_else(|| mlua::Error::runtime("Scheduler not found in app data container"))
}
