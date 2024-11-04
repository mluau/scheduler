use crate::{util::is_poll_pending, JoinHandles, ThreadHandle};
use std::time::Duration;

async fn lua_spawn(
    lua: mlua::Lua,
    (func, args): (mlua::Function, mlua::MultiValue),
) -> mlua::Result<usize> {
    let mut join_handles = lua.app_data_mut::<JoinHandles>().unwrap();
    let thread = lua.create_thread(func).unwrap();

    match thread.resume::<mlua::MultiValue>(args.clone()) {
        Ok(v) => {
            if v.get(0).is_some_and(is_poll_pending) {
                join_handles.0.push(ThreadHandle {
                    tokio: Some(tokio::spawn(async move {
                        let stream = thread.into_async::<()>(args);

                        stream.await.expect("Failed to run spawned thread");
                    })),
                });
            } else {
                join_handles.0.push(ThreadHandle { tokio: None });
            }
        }
        Err(err) => {
            join_handles.0.push(ThreadHandle { tokio: None });

            panic!("{err}")
        }
    };

    Ok(join_handles.0.len())
}

async fn lua_cancel(lua: mlua::Lua, thread_id: usize) -> mlua::Result<()> {
    let mut join_handles = lua.app_data_mut::<JoinHandles>().unwrap();
    let handle = join_handles
        .0
        .get_mut(thread_id - 1)
        .expect("Failed to get thread");

    if let Some(tokio_handle) = handle.tokio.take() {
        tokio_handle.abort();
    }

    Ok(())
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
            .create_async_function(lua_cancel)
            .expect("Failed to create cancel function");

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
