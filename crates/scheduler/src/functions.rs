fn lua_spawn(
    lua: &mlua::Lua,
    (func, args): (mlua::Either<mlua::Function, mlua::Thread>, mlua::MultiValue),
) -> mlua::Result<mlua::Thread> {
    let thread = func
        .map_left(|x| {
            lua.create_thread(x)
                .expect("Failed to turn function into thread")
        })
        .into_inner();

    crate::spawn_thread(&lua, thread, crate::SpawnProt::Spawn, args)
}

fn lua_defer(
    lua: &mlua::Lua,
    (func, args): (mlua::Either<mlua::Function, mlua::Thread>, mlua::MultiValue),
) -> mlua::Result<mlua::Thread> {
    let thread = func
        .map_left(|x| {
            lua.create_thread(x)
                .expect("Failed to turn function into thread")
        })
        .into_inner();

    crate::spawn_thread(&lua, thread, crate::SpawnProt::Defer, args)
}

async fn lua_cancel(lua: mlua::Lua, thread: mlua::Thread) -> mlua::Result<()> {
    crate::cancel_thread(&lua, thread).await
}

async fn lua_yield(lua: mlua::Lua, _: ()) -> mlua::Result<mlua::MultiValue> {
    crate::yield_thread(&lua, lua.current_thread()).await
}

pub struct Functions {
    pub spawn: mlua::Function,
    pub defer: mlua::Function,
    pub cancel: mlua::Function,
    pub yield_: mlua::Function,
}

impl Functions {
    pub fn new(lua: &mlua::Lua) -> mlua::Result<Self> {
        let spawn = lua.create_function(lua_spawn)?;
        let defer = lua.create_function(lua_defer)?;
        let cancel = lua.create_async_function(lua_cancel)?;
        let yield_ = lua.create_async_function(lua_yield)?;

        Ok(Self {
            spawn,
            defer,
            cancel,
            yield_,
        })
    }
}
