use mlua::prelude::*;

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

            taskmgr_wq_ref.feedback().on_thread_add(
                "WaitingThread.WaitSemantics",
                &curr_thread,
                &th,
            )?;

            taskmgr_wq_ref.add_waiting_thread(
                th,
                None,
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

                taskmgr_delay_ref.feedback().on_thread_add(
                    "WaitingThread.DelaySemantics",
                    &lua.current_thread(),
                    &th,
                )?;

                taskmgr_delay_ref.add_waiting_thread(
                    th.clone(),
                    Some(args),
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

    // Adds a thread to the deferred queue
    let taskmgr_back_ref = taskmgr_parent.clone();
    scheduler_tab.set(
        "addDeferred",
        lua.create_function(
            move |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                taskmgr_back_ref.feedback().on_thread_add(
                    "DeferredThread",
                    &lua.current_thread(),
                    &th,
                )?;

                taskmgr_back_ref.add_deferred_thread(th.clone(), args);
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

    scheduler_tab.set(
        "onThreadAdd",
        lua.create_function(move |lua, (label, th): (String, LuaThread)| {
            let taskmgr = super::taskmgr::get(lua);
            taskmgr.feedback().on_thread_add(&label, &lua.current_thread(), &th)
            .map_err(LuaError::external)?;
            Ok(())
        })?,
    )?;

    let taskmgr = taskmgr_parent.clone();
    scheduler_tab.set(
        "onResponseOk",
        lua.create_function(move |lua, (label, th, result): (String, LuaThread, LuaMultiValue)| {
            taskmgr.feedback().on_response(&label, &th, Ok(result));
            Ok(())
        })?,
    )?;

    let taskmgr = taskmgr_parent.clone();
    scheduler_tab.set(
        "onResponseErr",
        lua.create_function(move |lua, (label, th, result): (String, LuaThread, LuaError)| {
            taskmgr.feedback().on_response(&label, &th, Err(result));
            Ok(())
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

local old_create = coroutine.create
local old_resume = coroutine.resume
local function coroutineResume(coroutine, ...)
    local ok, v = old_resume(coroutine, ...)
    if ok then
        table.onResponseOk("TaskSpawn", coroutine, v)
    else
        table.onResponseErr("TaskSpawn", coroutine, v)
    end
end

local function coroutineCreate(task, ...)
    local thread = old_create(task, ...)
    table.onThreadAdd("TaskSpawn", thread)
    return thread
end

coroutine.create = coroutineCreate
coroutine.resume = coroutineResume

local function defer<T...>(task: Task<T...>, ...: T...): thread
    return table.addDeferred(task, ...)
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

local function spawn(task: TaskFunction, ...: any): thread
    local thread = if type(task) == "thread" then task else coroutine.create(task)
    coroutine.resume(thread, ...)
    return thread
end

return {
    defer = defer,
    delay = delay,
    desynchronize = desynchronize,
    synchronize = synchronize,
    wait = wait,
    cancel = cancel,
    spawn = spawn,
}"#,
        )
        .set_name("task")
        .set_environment(lua.globals())
        .call::<LuaTable>(scheduler_tab)?;

    table.set_readonly(true);

    Ok(table)
}
