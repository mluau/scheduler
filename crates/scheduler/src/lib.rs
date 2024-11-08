use flume::bounded;
use mlua::ExternalResult;
use scheduler::Scheduler;
use smol::Task;
use std::{future::Future, sync::Arc};

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

    let (sender, receiver) = bounded(1);

    scheduler
        .yield_pool
        .0
        .send_async((thread.to_pointer() as usize, sender))
        .await
        .into_lua_err()?;

    receiver.recv_async().await.into_lua_err()
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

    let scheduler = Arc::clone(&lua.app_data_ref::<Arc<Scheduler>>().unwrap());

    let args = args.into_lua_multi(lua)?;
    let thread_inner = thread.clone();
    let args_inner = args.clone();
    let thread_id = thread_inner.to_pointer() as _;
    let thread_info = ThreadInfo(thread_inner, args_inner);

    let send_to_sched = if matches!(prot, SpawnProt::Spawn) {
        // poll immediately
        if let Some(result) = tick_thread(&thread_info) {
            // thread already finished
            let scheduler_inner = Arc::clone(&scheduler);

            spawn_future(&lua.clone(), async move {
                scheduler_inner
                    .result_pool
                    .0
                    .send_async((thread_id, result))
                    .await
                    .expect("Failed to send thread to scheduler")
            })
            .detach();

            false
        } else {
            true
        }
    } else {
        true
    };

    if send_to_sched {
        spawn_future(&lua.clone(), async move {
            scheduler
                .spawn_pool
                .0
                .send_async((thread_id, thread_info))
                .await
                .expect("Failed to send thread to scheduler")
        })
        .detach();
    }

    Ok(thread)
}

async fn await_thread(lua: &mlua::Lua, thread: mlua::Thread) -> mlua::Result<mlua::MultiValue> {
    let scheduler = Arc::clone(&lua.app_data_ref::<Arc<Scheduler>>().unwrap());

    let thread_id = thread.to_pointer() as _;
    let (sender, receiver) = bounded(1);

    spawn_future(&lua.clone(), async move {
        scheduler
            .result_sender_pool
            .0
            .send_async((thread_id, sender))
            .await
            .expect("Failed to send thread to scheduler")
    })
    .detach();

    receiver.recv_async().await.into_lua_err()?
}

async fn cancel_thread(lua: &mlua::Lua, thread: mlua::Thread) -> mlua::Result<()> {
    let scheduler = Arc::clone(&lua.app_data_ref::<Arc<Scheduler>>().unwrap());
    let thread_id = thread.to_pointer() as _;

    scheduler
        .cancel_pool
        .0
        .send_async(thread_id)
        .await
        .into_lua_err()
}

fn tick_thread(thread_info: &ThreadInfo) -> Option<mlua::Result<mlua::MultiValue>> {
    if let mlua::ThreadStatus::Resumable = thread_info.0.status() {
        let result = thread_info
            .0
            .resume::<mlua::MultiValue>(thread_info.1.clone());

        if let mlua::ThreadStatus::Resumable = thread_info.0.status() {
            None
        } else {
            Some(result)
        }
    } else {
        None
    }
}
