use std::time::Duration;
use tokio::time::Instant;

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

    crate::spawn_local(&lua, thread, args)
}

async fn lua_wait(_lua: mlua::Lua, amount: Option<f64>) -> mlua::Result<f64> {
    let duration = Duration::from_secs_f64(amount.unwrap_or_default());
    let started = Instant::now();

    tokio::time::sleep(duration).await;

    Ok((Instant::now() - started).as_secs_f64())
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
