use mlua::prelude::*;

/// Patches the coroutine library to both work with the scheduler properly, but also to be more sane without deadlocking
pub fn patch_coroutine_lib(lua: &Lua) -> LuaResult<()> {
    let coroutine = lua.globals().get::<LuaTable>("coroutine")?;

    // coroutine.resume needs to tell the scheduler its pending for resumption
    coroutine.set(
        "resume",
        lua.create_async_function(|lua, (th, args): (LuaThread, LuaMultiValue)| async move {
            let taskmgr = super::taskmgr::get(&lua);
            match taskmgr
                .resume_thread("CoroutineResume", th, args.clone())
                .await
            {
                Ok(res) => (true, res).into_lua_multi(&lua),
                Err(err) => {
                    // Error, return false and error message
                    (false, err.to_string()).into_lua_multi(&lua)
                }
            }
        })?,
    )?;

    Ok(())
}

pub fn table(lua: &Lua) -> LuaResult<LuaTable> {
    let table = lua
        .load(
            r#"
local table = {}

local function defer<T...>(task: Task<T...>, ...: T...): thread
    return table.__addDeferred(task, ...)
end

local function delay<T...>(time: number, task: Task<T...>, ...: T...): thread
    return table.__addWaitingWithArgs(task, time, nil, ...)
end

local function desynchronize(...)
    return
end

local function synchronize(...)
    return
end

local function wait(time: number?): number
    if time == nil then
        time = 0
    end
    table.__addWaiting(coroutine.running(), time)
    coroutine.yield()
    return os.clock() - time
end

local function cancel(thread: thread): ()
	coroutine.close(thread)
end

table.defer = defer
table.delay = delay
table.desynchronize = desynchronize
table.synchronize = synchronize
table.wait = wait
table.cancel = cancel

return table
        "#,
        )
        .set_environment(lua.globals())
        .call::<LuaTable>(())?;

    table.set(
        "__addWaiting",
        lua.create_function(|lua, (th, resume): (LuaThread, f64)| {
            let scheduler = super::taskmgr::get(lua);
            scheduler.add_waiting_thread(
                th,
                LuaMultiValue::new(),
                std::time::Duration::from_secs_f64(resume),
            );
            Ok(())
        })?,
    )?;

    table.set(
        "__addWaitingWithArgs",
        lua.create_function(
            |lua, (f, resume, args): (LuaEither<LuaFunction, LuaThread>, f64, LuaMultiValue)| {
                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let scheduler = super::taskmgr::get(lua);
                scheduler.add_waiting_thread(
                    th.clone(),
                    args,
                    std::time::Duration::from_secs_f64(resume),
                );
                Ok(th)
            },
        )?,
    )?;

    table.set(
        "__addDeferred",
        lua.create_function(
            |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let scheduler = super::taskmgr::get(lua);
                scheduler.add_deferred_thread(th.clone(), args);
                Ok(th)
            },
        )?,
    )?;

    table.set(
        "spawn",
        lua.create_function(
            |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let t = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                t.resume::<()>(args)?;

                Ok(t)
            },
        )?,
    )?;
    Ok(table)
}
