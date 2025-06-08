use mlua::prelude::*;

/// Creates the `task` library, patching coroutine.resume to handle on_response signals as appropriate.
pub fn task_lib(lua: &Lua) -> LuaResult<LuaTable> {
    let taskmgr_parent = super::taskmgr::get(lua);

    let taskmgr_ref = taskmgr_parent.clone();
    lua.globals().get::<LuaTable>("coroutine")?.set(
        "resume",
        lua.create_function(move |_lua, (coroutine, args): (LuaThread, LuaMultiValue)| {
            let result = coroutine.resume(args);
            taskmgr_ref
                .feedback()
                .on_response("CoroutineResume", &coroutine, result.clone());
            result
        })?,
    )?;

    let table = lua.create_table()?;
    let taskmgr_ref = taskmgr_parent.clone();
    table.set(
        "defer",
        lua.create_function(
            move |lua, (task, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let thread = match task {
                    LuaEither::Left(func) => lua.create_thread(func)?,
                    LuaEither::Right(th) => th,
                };

                taskmgr_ref.add_deferred_thread(thread.clone(), args);
                println!("Task deferred: {:?}", thread);
                Ok(thread)
            },
        )?,
    )?;

    let taskmgr_ref = taskmgr_parent.clone();
    table.set(
        "delay",
        lua.create_function(
            move |lua, (time, task, args): (f64, LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let thread = match task {
                    LuaEither::Left(func) => lua.create_thread(func)?,
                    LuaEither::Right(th) => th,
                };

                taskmgr_ref.add_waiting_thread(
                    thread.clone(),
                    Some(args),
                    std::time::Duration::from_secs_f64(time),
                );
                Ok(thread)
            },
        )?,
    )?;

    table.set(
        "desynchronize",
        lua.create_function(move |_lua, _args: LuaMultiValue| {
            // No-op in this context
            Ok(())
        })?,
    )?;

    table.set(
        "synchronize",
        lua.create_function(move |_lua, _args: LuaMultiValue| {
            // No-op in this context
            Ok(())
        })?,
    )?;

    let taskmgr_ref = taskmgr_parent.clone();
    table.set(
        "wait",
        lua.create_function(move |lua, time: Option<f64>| {
            let thread = lua.current_thread();
            let duration = time.map_or(std::time::Duration::from_secs_f64(0.05), |t| {
                std::time::Duration::from_secs_f64(t.max(0.05))
            });

            taskmgr_ref.add_waiting_thread(thread.clone(), None, duration);
            lua.yield_with(())
        })?,
    )?;

    let taskmgr_ref = taskmgr_parent.clone();
    let coro_close_fn = lua
        .globals()
        .get::<LuaTable>("coroutine")?
        .get::<LuaFunction>("close")?;

    table.set(
        "cancel",
        lua.create_function(move |_lua, thread: LuaThread| {
            taskmgr_ref.cancel_task(&thread);
            coro_close_fn.call::<()>(thread)?;
            Ok(())
        })?,
    )?;

    let taskmgr_ref = taskmgr_parent.clone();
    table.set(
        "spawn",
        lua.create_function(
            move |lua, (task, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let thread = match task {
                    LuaEither::Left(func) => lua.create_thread(func)?,
                    LuaEither::Right(th) => th,
                };

                let result = thread.resume(args);
                taskmgr_ref
                    .feedback()
                    .on_response("TaskSpawn", &thread, result.clone());
                Ok(thread)
            },
        )?,
    )?;

    table.set_readonly(true);

    Ok(table)
}
