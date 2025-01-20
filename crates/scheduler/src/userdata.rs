use mlua::prelude::*;

/// Patches the coroutine library to both work with the scheduler properly, but also to be more sane without deadlocking
pub fn patch_coroutine_lib(lua: &Lua) -> LuaResult<()> {
    let coroutine = lua.globals().get::<LuaTable>("coroutine")?;

    // Custom coroutine.create that also sends on_thread_add
    coroutine.set(
        "create",
        lua.create_function(|lua, f: LuaFunction| {
            let taskmgr = super::taskmgr::get(lua);

            let curr_thread = lua.current_thread();

            let th = lua.create_thread(f)?;

            taskmgr
                .inner
                .feedback
                .on_thread_add("CoroutineCreate", &curr_thread, &th)?;

            Ok(th)
        })?,
    )?;

    // Note: this function is special [hence why it doesn't use schedulers async polling system]
    #[cfg(not(feature = "fast"))]
    coroutine.set(
        "resume",
        lua.create_async_function(|lua, (th, args): (LuaThread, LuaMultiValue)| async move {
            let taskmgr = super::taskmgr::get(&lua);

            taskmgr
                .inner
                .feedback
                .on_thread_add("CoroutineResume", &lua.current_thread(), &th)?;

            let res = taskmgr.resume_thread_slow(th.clone(), args).await;

            taskmgr
                .inner
                .feedback
                .on_response("CoroutineResume", &taskmgr, &th, res.clone());

            match res {
                Some(Ok(res)) => (true, res).into_lua_multi(&lua),
                Some(Err(err)) => {
                    // Error, return false and error message
                    (false, err.to_string()).into_lua_multi(&lua)
                }
                None => (true, LuaMultiValue::new()).into_lua_multi(&lua),
            }
        })?,
    )?;

    #[cfg(feature = "fast")]
    coroutine.set(
        "resume",
        lua.create_function(|lua, (th, args): (LuaThread, LuaMultiValue)| {
            let taskmgr = super::taskmgr::get(lua);

            taskmgr
                .inner
                .feedback
                .on_thread_add("CoroutineResume", &lua.current_thread(), &th)?;

            let res = taskmgr.resume_thread_fast(&th, args);

            taskmgr
                .inner
                .feedback
                .on_response("CoroutineResume", &taskmgr, &th, res.clone());

            match res {
                Some(Ok(res)) => (true, res).into_lua_multi(lua),
                Some(Err(err)) => {
                    // Error, return false and error message
                    (false, err.to_string()).into_lua_multi(lua)
                }
                None => (true, LuaMultiValue::new()).into_lua_multi(lua),
            }
        })?,
    )?;

    // Patch wrap implementation
    lua.load(
        r#"
coroutine.wrap = function(...)
    local t = coroutine.create(...)
    local r = { coroutine.resume(t, ...) }
    if r[1] then
        return select(2, unpack(r))
    else
        error(r[2], 2)
    end
end"#,
    )
    .set_name("__sched_wrap")
    .call::<()>(())?;

    Ok(())
}

/// Returns the low-level Scheduler library of which task lib is based on
pub fn scheduler_lib(lua: &Lua) -> LuaResult<LuaTable> {
    let scheduler_tab = lua.create_table()?;

    // Adds a thread to the waiting queue
    scheduler_tab.set(
        "addWaitingWait",
        lua.create_function(|lua, (th, resume): (LuaThread, f64)| {
            if resume < 0.05 {
                return Err(LuaError::RuntimeError(
                    "Cannot wait for less than 0.05 seconds".to_string(),
                ));
            }

            let taskmgr = super::taskmgr::get(lua);

            let curr_thread = lua.current_thread();

            taskmgr.inner.feedback.on_thread_add(
                "WaitingThread.WaitSemantics",
                &curr_thread,
                &th,
            )?;

            taskmgr.add_waiting_thread(
                th,
                super::taskmgr::WaitOp::Wait,
                std::time::Duration::from_secs_f64(resume),
            );
            Ok(())
        })?,
    )?;

    // Adds a thread to the waiting queue with arguments
    scheduler_tab.set(
        "addWaitingDelay",
        lua.create_function(
            |lua, (f, resume, args): (LuaEither<LuaFunction, LuaThread>, f64, LuaMultiValue)| {
                if resume < 0.05 {
                    return Err(LuaError::RuntimeError(
                        "Cannot wait for less than 0.05 seconds".to_string(),
                    ));
                }

                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let taskmgr = super::taskmgr::get(lua);

                taskmgr.inner.feedback.on_thread_add(
                    "WaitingThread.DelaySemantics",
                    &lua.current_thread(),
                    &th,
                )?;

                taskmgr.add_waiting_thread(
                    th.clone(),
                    super::taskmgr::WaitOp::Delay { args },
                    std::time::Duration::from_secs_f64(resume),
                );
                Ok(th)
            },
        )?,
    )?;

    // Removes a thread from the waiting queue, returning the number of threads removed
    scheduler_tab.set(
        "removeWaiting",
        lua.create_function(|lua, th: LuaThread| {
            let taskmgr = super::taskmgr::get(lua);

            Ok(taskmgr.remove_waiting_thread(&th))
        })?,
    )?;

    // Adds a thread to the deferred queue at the front
    scheduler_tab.set(
        "addDeferredFront",
        lua.create_function(
            |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let taskmgr = super::taskmgr::get(lua);

                taskmgr.inner.feedback.on_thread_add(
                    "DeferredThread",
                    &lua.current_thread(),
                    &th,
                )?;

                taskmgr.add_deferred_thread_front(th.clone(), args);
                Ok(th)
            },
        )?,
    )?;

    // Adds a thread to the deferred queue at the back
    scheduler_tab.set(
        "addDeferredBack",
        lua.create_function(
            |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let taskmgr = super::taskmgr::get(lua);

                taskmgr.inner.feedback.on_thread_add(
                    "DeferredThread",
                    &lua.current_thread(),
                    &th,
                )?;

                taskmgr.add_deferred_thread_back(th.clone(), args);
                Ok(th)
            },
        )?,
    )?;

    // Removes a thread from the deferred queue returning the number of threads removed
    scheduler_tab.set(
        "removeDeferred",
        lua.create_function(|lua, th: LuaThread| {
            let taskmgr = super::taskmgr::get(lua);

            Ok(taskmgr.remove_deferred_thread(&th))
        })?,
    )?;

    scheduler_tab.set_readonly(true);

    Ok(scheduler_tab)
}

/// Returns an implementation of the `task` library as a table
pub fn task_lib(lua: &Lua, scheduler_tab: LuaTable) -> LuaResult<LuaTable> {
    let table = lua
        .load(
            r#"
local table = ...

local function defer<T...>(task: Task<T...>, ...: T...): thread
    return table.addDeferredFront(task, ...)
end

local function delay<T...>(time: number, task: Task<T...>, ...: T...): thread
    if time == nil or time < 0.05 then
        time = 0.05 -- Avoid 100% CPU usage
    end
    return table.addWaitingDelay(task, time, ...)
end

local function desynchronize(...)
    return
end

local function synchronize(...)
    return
end

local function wait(time: number?): number
    if time == nil or time < 0.05 then
        time = 0.05 -- Avoid 100% CPU usage
    end
    table.addWaitingWait(coroutine.running(), time)
    return coroutine.yield()
end

local function cancel(thread: thread): ()
    table.removeWaiting(thread)
    table.removeDeferred(thread)
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
    #[cfg(not(feature = "fast"))]
    table.set(
        "spawn",
        lua.create_async_function(
            |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| async move {
                let t = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let taskmgr = super::taskmgr::get(&lua);

                taskmgr
                    .inner
                    .feedback
                    .on_thread_add("TaskSpawn", &lua.current_thread(), &t)?;

                let result = taskmgr.resume_thread_slow(t.clone(), args).await;

                taskmgr
                    .inner
                    .feedback
                    .on_response("TaskSpawn", &taskmgr, &t, result);

                Ok(t)
            },
        )?,
    )?;

    #[cfg(feature = "fast")]
    table.set(
        "spawn",
        lua.create_function(
            |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let t = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let taskmgr = super::taskmgr::get(lua);

                taskmgr
                    .inner
                    .feedback
                    .on_thread_add("TaskSpawn", &lua.current_thread(), &t)?;

                let result = taskmgr.resume_thread_fast(&t, args);

                taskmgr
                    .inner
                    .feedback
                    .on_response("TaskSpawn", &taskmgr, &t, result);

                Ok(t)
            },
        )?,
    )?;

    Ok(table)
}
