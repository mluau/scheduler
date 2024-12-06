use mlua::prelude::*;

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
                super::heartbeat::secs_to_ticks(resume),
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
                    super::heartbeat::secs_to_ticks(resume),
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
