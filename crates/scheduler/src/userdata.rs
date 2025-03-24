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
///
/// Note that task manager must be attached prior to calling this function
pub fn scheduler_lib(lua: &Lua) -> LuaResult<LuaTable> {
    let taskmgr_parent = super::taskmgr::get(lua);

    let scheduler_tab = lua.create_table()?;

    // Adds a thread to the waiting queue
    let taskmgr_wq_ref = taskmgr_parent.clone();
    scheduler_tab.set(
        "addWaitingWait",
        lua.create_function(move |lua, (th, resume): (LuaThread, Option<f64>)| {
            let mut resume = resume.unwrap_or_default();
            if resume < 0.05 {
                resume = 0.05; // Avoid 100% CPU usage
            }

            let curr_thread = lua.current_thread();

            taskmgr_wq_ref.inner.feedback.on_thread_add(
                "WaitingThread.WaitSemantics",
                &curr_thread,
                &th,
            )?;

            taskmgr_wq_ref.add_waiting_thread(
                th,
                super::taskmgr::WaitOp::Wait,
                std::time::Duration::from_secs_f64(resume),
            );
            Ok(())
        })?,
    )?;

    // Adds a thread to the waiting queue with arguments
    let taskmgr_delay_ref = taskmgr_parent.clone();
    scheduler_tab.set(
        "addWaitingDelay",
        lua.create_function(
            move |lua,
                  (f, resume, args): (
                LuaEither<LuaFunction, LuaThread>,
                Option<f64>,
                LuaMultiValue,
            )| {
                let mut resume = resume.unwrap_or_default();
                if resume < 0.05 {
                    resume = 0.05; // Avoid 100% CPU usage
                }

                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                taskmgr_delay_ref.inner.feedback.on_thread_add(
                    "WaitingThread.DelaySemantics",
                    &lua.current_thread(),
                    &th,
                )?;

                taskmgr_delay_ref.add_waiting_thread(
                    th.clone(),
                    super::taskmgr::WaitOp::Delay { args },
                    std::time::Duration::from_secs_f64(resume),
                );
                Ok(th)
            },
        )?,
    )?;

    // Removes a thread from the waiting queue, returning the number of threads removed
    let taskmgr_remove_ref = taskmgr_parent.clone();
    scheduler_tab.set(
        "removeWaiting",
        lua.create_function(move |_lua, th: LuaThread| {
            Ok(taskmgr_remove_ref.remove_waiting_thread(&th))
        })?,
    )?;

    // Adds a thread to the deferred queue at the front
    let taskmgr_front_ref = taskmgr_parent.clone();
    scheduler_tab.set(
        "addDeferredFront",
        lua.create_function(
            move |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                taskmgr_front_ref.inner.feedback.on_thread_add(
                    "DeferredThread",
                    &lua.current_thread(),
                    &th,
                )?;

                taskmgr_front_ref.add_deferred_thread_front(th.clone(), args);
                Ok(th)
            },
        )?,
    )?;

    // Adds a thread to the deferred queue at the back
    let taskmgr_back_ref = taskmgr_parent.clone();
    scheduler_tab.set(
        "addDeferredBack",
        lua.create_function(
            move |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                taskmgr_back_ref.inner.feedback.on_thread_add(
                    "DeferredThread",
                    &lua.current_thread(),
                    &th,
                )?;

                taskmgr_back_ref.add_deferred_thread_back(th.clone(), args);
                Ok(th)
            },
        )?,
    )?;

    // Removes a thread from the deferred queue returning the number of threads removed
    let taskmgr_remove_deferred_ref = taskmgr_parent.clone();
    scheduler_tab.set(
        "removeDeferred",
        lua.create_function(move |_lua, th: LuaThread| {
            Ok(taskmgr_remove_deferred_ref.remove_deferred_thread(&th))
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
    return table.addWaitingDelay(task, time, ...)
end

local function desynchronize(...)
    return
end

local function synchronize(...)
    return
end

local function wait(time: number?): number
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
        .set_name("task")
        .set_environment(lua.globals())
        .call::<LuaTable>(scheduler_tab)?;

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

                let result = t.resume(args);

                taskmgr
                    .inner
                    .feedback
                    .on_response("TaskSpawn", &taskmgr, &t, result);

                Ok(t)
            },
        )?,
    )?;

    table.set_readonly(true);

    Ok(table)
}
