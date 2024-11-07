use mlua::ExternalResult;
use scheduler::Scheduler;
use smol::{channel, Task};
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
