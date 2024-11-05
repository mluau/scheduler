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

    crate::spawn_local(&lua, thread, crate::SpawnProt::Spawn, args).await
}

async fn lua_defer(
    lua: mlua::Lua,
    (func, args): (mlua::Either<mlua::Function, mlua::Thread>, mlua::MultiValue),
) -> mlua::Result<mlua::Thread> {
    let thread = func
        .map_left(|x| {
            lua.create_thread(x)
                .expect("Failed to turn function into thread")
        })
        .into_inner();

    crate::spawn_local(&lua, thread, crate::SpawnProt::Defer, args).await
}

pub struct Functions {
    pub spawn: mlua::Function,
    pub defer: mlua::Function,
    pub cancel: mlua::Function,
}

impl Functions {
    pub fn new(lua: &mlua::Lua) -> mlua::Result<Self> {
        let spawn = lua
            .create_async_function(lua_spawn)
            .expect("Failed to create spawn function");

        let defer = lua
            .create_async_function(lua_defer)
            .expect("Failed to create spawn function");

        let cancel = lua
            .globals()
            .get::<mlua::Table>("coroutine")?
            .get::<mlua::Function>("close")?;

        Ok(Self {
            spawn,
            defer,
            cancel,
        })
    }
}
