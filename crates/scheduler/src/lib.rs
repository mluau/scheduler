// use std::time::Duration;

use async_task::Runnable;
use smol::future::block_on;

pub mod functions;
pub(crate) mod util;

#[derive(Debug)]
pub struct Scheduler {
    pub(crate) spawn_channel: (flume::Sender<Runnable>, flume::Receiver<Runnable>),
    pub(crate) defer_channel: (flume::Sender<Runnable>, flume::Receiver<Runnable>),

    pub errors: Vec<mlua::Error>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            spawn_channel: flume::unbounded(),
            defer_channel: flume::unbounded(),
            errors: Vec::new(),
        }
    }
}

pub fn setup_scheduler(lua: &mlua::Lua) {
    lua.set_app_data(Scheduler::default());
}

pub enum SpawnProt {
    Spawn,
    Defer,
}

pub async fn spawn_local<A: mlua::IntoLuaMulti>(
    lua: &mlua::Lua,
    thread: mlua::Thread,
    prot: SpawnProt,
    args: A,
) -> mlua::Result<mlua::Thread> {
    let thread_inner = thread.clone();
    let args = args.into_lua_multi(lua)?;

    let sender = {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();
        match prot {
            SpawnProt::Spawn => scheduler.spawn_channel.0.clone(),
            SpawnProt::Defer => scheduler.defer_channel.0.clone(),
        }
    };

    let lua_inner = lua.clone();
    let fake_yield = if matches!(prot, SpawnProt::Defer) {
        false
    } else {
        match thread.resume::<mlua::MultiValue>(args.clone()) {
            Ok(v) => {
                if v.get(0).is_some_and(util::is_poll_pending) {
                    false
                } else {
                    true
                }
            }
            Err(_) => true,
        }
    };

    let (runnable, task) = async_task::spawn(
        async move {
            match thread_inner.status() {
                mlua::ThreadStatus::Resumable => {
                    if fake_yield {
                        smol::future::yield_now().await;
                    }

                    let stream = thread_inner.into_async::<()>(args);

                    if let Err(err) = stream.await {
                        eprintln!("{err}");

                        let mut scheduler = lua_inner.app_data_mut::<Scheduler>().unwrap();
                        scheduler.errors.push(err.clone());
                    };
                }
                _ => {}
            }
        },
        move |runnable| {
            sender.send(runnable).unwrap();
        },
    );

    runnable.schedule();
    task.detach();

    Ok(thread)
}

pub async fn await_scheduler(lua: &mlua::Lua) -> Scheduler {
    smol::future::yield_now().await;

    let (spawn_recv, defer_recv) = {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();
        (
            scheduler.spawn_channel.1.clone(),
            scheduler.defer_channel.1.clone(),
        )
    };

    block_on(async move {
        loop {
            if spawn_recv.sender_count() > 1 {
                while let Ok(runnable) = spawn_recv.try_recv() {
                    runnable.run();

                    if spawn_recv.sender_count() == 1 {
                        break;
                    }
                }
            }

            if defer_recv.sender_count() > 1 {
                while let Ok(runnable) = defer_recv.try_recv() {
                    runnable.run();

                    if spawn_recv.sender_count() == 1 {
                        break;
                    }
                }
            }

            if spawn_recv.sender_count() == 1 && defer_recv.sender_count() == 1 {
                break;
            }

            smol::future::yield_now().await;
        }
    });

    lua.remove_app_data::<Scheduler>().unwrap()
}
