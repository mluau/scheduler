use async_task::Runnable;
use smol::future::block_on;
use std::sync::{Arc, Mutex};

pub mod functions;
pub(crate) mod util;

#[derive(Debug)]
pub struct Scheduler {
    pub(crate) spawn_channel: (
        smol::channel::Sender<Runnable>,
        smol::channel::Receiver<Runnable>,
    ),
    pub(crate) defer_channel: (
        smol::channel::Sender<Runnable>,
        smol::channel::Receiver<Runnable>,
    ),

    pub(crate) threads: Arc<Mutex<Vec<mlua::Thread>>>,
    pub errors: Vec<mlua::Error>,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self {
            spawn_channel: smol::channel::unbounded(),
            defer_channel: smol::channel::unbounded(),

            threads: Default::default(),
            errors: Default::default(),
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
        scheduler.threads.lock().unwrap().push(thread.clone());
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
        move |runnable: Runnable| {
            // let waker = runnable.waker();

            if let Err(err) = sender.send_blocking(runnable) {
                eprintln!("{err}");
            }

            // if let Err(err) = sender_inner.lock().unwrap().try_send(runnable) {
            //     if matches!(err, smol::channel::TrySendError::Full(_)) {
            //         waker.wake();
            //     } else {
            //         eprintln!("{err}");
            //     }
            // }
        },
    );

    runnable.schedule();
    task.detach();

    Ok(thread)
}

pub async fn await_scheduler(lua: &mlua::Lua) -> Scheduler {
    smol::future::yield_now().await;

    let (threads, spawn_recv, defer_recv) = {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();
        (
            Arc::clone(&scheduler.threads),
            scheduler.spawn_channel.1.clone(),
            scheduler.defer_channel.1.clone(),
        )
    };

    block_on(async move {
        'main: loop {
            while let Ok(runnable) = spawn_recv.try_recv() {
                smol::spawn(async move {
                    runnable.run();
                })
                .detach();
            }

            while let Ok(runnable) = defer_recv.try_recv() {
                smol::spawn(async move {
                    runnable.run();
                })
                .detach();
            }

            for thread in threads.lock().unwrap().iter() {
                if !matches!(thread.status(), mlua::ThreadStatus::Finished) {
                    continue 'main;
                }
            }

            break 'main;
        }
    });

    lua.remove_app_data::<Scheduler>().unwrap()
}
