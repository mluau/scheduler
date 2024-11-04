use crate::{util::is_poll_pending, Scheduler, ThreadHandle};
use std::time::Duration;

async fn lua_spawn(
    lua: mlua::Lua,
    (func, args): (mlua::Either<mlua::Function, mlua::Thread>, mlua::MultiValue),
) -> mlua::Result<mlua::Thread> {
    let thread = func
        .map_left(|x| {
            lua.create_thread(x)
                .expect("Failed to turn function into thread")
        })
        .into_inner();
    let thread_inner = thread.clone();

    {
        let mut scheduler = lua.app_data_mut::<Scheduler>().unwrap();
        scheduler.handles.push(ThreadHandle {
            thread: thread.clone(),
        });
    }

    match thread.resume::<mlua::MultiValue>(args.clone()) {
        Ok(v) => {
            if v.get(0).is_some_and(is_poll_pending) {
                let lua_inner = lua.clone();
                tokio::spawn(async move {
                    match thread_inner.status() {
                        mlua::ThreadStatus::Resumable => {
                            let stream = thread_inner.into_async::<()>(args);

                            if let Err(err) = stream.await {
                                eprintln!("{err}");

                                let mut scheduler = lua_inner.app_data_mut::<Scheduler>().unwrap();
                                scheduler.errors.push(err.clone());
                            };
                        }
                        _ => {}
                    }
                });
            }
        }
        Err(err) => {
            let mut scheduler = lua.app_data_mut::<Scheduler>().unwrap();
            scheduler.errors.push(err.clone());

            eprintln!("{err}");
        }
    };

    Ok(thread)
}

async fn lua_wait(_lua: mlua::Lua, amount: f64) -> mlua::Result<()> {
    tokio::time::sleep(Duration::from_secs_f64(amount)).await;

    Ok(())
}

pub struct Functions {
    pub spawn: mlua::Function,
    pub cancel: mlua::Function,

    pub wait: mlua::Function,
}

impl Functions {
    pub fn new(lua: &mlua::Lua) -> mlua::Result<Self> {
        let spawn = lua
            .create_async_function(lua_spawn)
            .expect("Failed to create spawn function");

        let cancel = lua
            .globals()
            .get::<mlua::Table>("coroutine")?
            .get::<mlua::Function>("close")?;

        let wait = lua
            .create_async_function(lua_wait)
            .expect("Failed to create wait function");

        Ok(Self {
            spawn,
            cancel,
            wait,
        })
    }

    pub fn into_dictionary(self, lua: &mlua::Lua) -> mlua::Result<mlua::Table> {
        let t = lua.create_table()?;

        t.set("spawn", self.spawn)?;
        t.set("cancel", self.cancel)?;

        t.set("wait", self.wait)?;

        Ok(t)
    }
}
