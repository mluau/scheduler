use mlua::prelude::*;

/// Patches the coroutine library to both work with the scheduler properly, but also to be more sane without deadlocking
pub fn patch_coroutine_lib(lua: &Lua) -> LuaResult<()> {
    let coroutine = lua.globals().get::<LuaTable>("coroutine")?;

    // Note: this function is special [hence why it doesn't use schedulers async polling system]
    coroutine.set(
        "resume",
        lua.create_async_function(|lua, (th, args): (LuaThread, LuaMultiValue)| async move {
            let taskmgr = super::taskmgr::get(&lua);

            #[cfg(feature = "thread_caller_tracking")]
            taskmgr
                .inner
                .feedback
                .on_thread_add(&lua.current_thread(), &th);

            let res = taskmgr
                .resume_thread("CoroutineResume", th.clone(), args)
                .await;

            taskmgr
                .inner
                .feedback
                .on_response("CoroutineResume", &taskmgr, &th, &res);

            match res {
                Ok(res) => (true, res).into_lua_multi(&lua),
                Err(err) => {
                    // Error, return false and error message
                    (false, err.to_string()).into_lua_multi(&lua)
                }
            }
        })?,
    )?;

    // Patch yield to error when it sees a error metatable
    lua.load(
        r#"
local ERROR_USERDATA = ...
local yield = coroutine.yield
coroutine.yield = function(...)
    local result = table.pack(yield(...))
    if rawequal(result[1], ERROR_USERDATA) then
        error(result[2])
    end
    return unpack(result, 1, result.n)
end
"#,
    )
    .set_name("__sched_yield")
    .call::<()>(
        lua.app_data_ref::<crate::taskmgr::ErrorUserdataValue>()
            .unwrap()
            .0
            .clone(),
    )?;

    Ok(())
}

pub fn table(lua: &Lua) -> LuaResult<LuaTable> {
    let scheduler_tab = lua.create_table()?;

    scheduler_tab.set(
        "__addWaiting",
        lua.create_function(|lua, (th, resume): (LuaThread, f64)| {
            let taskmgr = super::taskmgr::get(lua);

            #[cfg(feature = "thread_caller_tracking")]
            taskmgr
                .inner
                .feedback
                .on_thread_add(&lua.current_thread(), &th);

            taskmgr.add_waiting_thread(
                th,
                LuaMultiValue::new(),
                std::time::Duration::from_secs_f64(resume),
            );
            Ok(())
        })?,
    )?;

    scheduler_tab.set(
        "__addWaitingWithArgs",
        lua.create_function(
            |lua, (f, resume, args): (LuaEither<LuaFunction, LuaThread>, f64, LuaMultiValue)| {
                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let taskmgr = super::taskmgr::get(lua);

                #[cfg(feature = "thread_caller_tracking")]
                taskmgr
                    .inner
                    .feedback
                    .on_thread_add(&lua.current_thread(), &th);

                taskmgr.add_waiting_thread(
                    th.clone(),
                    args,
                    std::time::Duration::from_secs_f64(resume),
                );
                Ok(th)
            },
        )?,
    )?;

    scheduler_tab.set(
        "__addDeferred",
        lua.create_function(
            |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let taskmgr = super::taskmgr::get(lua);

                #[cfg(feature = "thread_caller_tracking")]
                taskmgr
                    .inner
                    .feedback
                    .on_thread_add(&lua.current_thread(), &th);

                taskmgr.add_deferred_thread(th.clone(), args);
                Ok(th)
            },
        )?,
    )?;

    let table = lua
        .load(
            r#"
local table = ...

local function defer<T...>(task: Task<T...>, ...: T...): thread
    return table.__addDeferred(task, ...)
end

local function delay<T...>(time: number, task: Task<T...>, ...: T...): thread
    return table.__addWaitingWithArgs(task, time, ...)
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
    return coroutine.yield()
end

local function cancel(thread: thread): ()
	coroutine.close(thread)
end

return {
    defer = defer,
    delay = delay,
    desynchronize = desynchronize,
    synchronize = synchronize,
    wait = wait,
    cancel = cancel
}"#,
        )
        .set_environment(lua.globals())
        .call::<LuaTable>(scheduler_tab)?;

    // task.spawn
    //
    // Note: this function is special [hence why it doesn't use schedulers async polling system]
    table.set(
        "spawn",
        lua.create_async_function(
            |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| async move {
                let t = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let taskmgr = super::taskmgr::get(&lua);

                #[cfg(feature = "thread_caller_tracking")]
                taskmgr
                    .inner
                    .feedback
                    .on_thread_add(&lua.current_thread(), &t);

                let result = taskmgr.resume_thread("ThreadSpawn", t.clone(), args).await;

                taskmgr
                    .inner
                    .feedback
                    .on_response("TaskResume", &taskmgr, &t, &result);

                Ok(t)
            },
        )?,
    )?;

    Ok(table)
}
