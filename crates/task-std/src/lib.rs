use mlua_scheduler::traits::LuaSchedulerMethods;
use std::time::{Duration, Instant};

// this implementation is taken from lune
const DELAY_LUA: &str = r"
return task.defer(function(...)
    task.wait(select(1, ...))
    task.spawn(select(2, ...))
end, ...)
";

pub fn inject_globals(lua: &mlua::Lua) -> mlua::Result<()> {
    let task_functions = mlua_scheduler::functions::Functions::new(&lua)?;

    let wait = lua.create_async_function(|_, secs: Option<f64>| async move {
        let now = Instant::now();
        smol::Timer::after(Duration::from_secs_f64(secs.unwrap_or_default())).await;
        Ok((Instant::now() - now).as_secs_f64())
    })?;

    let delay = lua.load(DELAY_LUA).set_name("delay").into_function()?;

    let task = lua.create_table()?;
    task.set("spawn", task_functions.spawn)?;
    task.set("defer", task_functions.defer)?;
    task.set("cancel", task_functions.cancel.clone())?;
    task.set("wait", wait)?;
    task.set("delay", delay)?;

    lua.globals().set("task", task)?;

    let wrap = lua.create_function(|lua, func: mlua::Function| {
        let thread = lua.create_thread(func)?;

        lua.create_function(move |lua: &mlua::Lua, args: mlua::MultiValue| {
            lua.spawn_thread(thread.clone(), mlua_scheduler::SpawnProt::Spawn, args)
        })
    })?;

    let resume = lua.create_function(|lua, (thread, args): (mlua::Thread, mlua::MultiValue)| {
        lua.spawn_thread(thread, mlua_scheduler::SpawnProt::Spawn, args)
    })?;

    let coroutine = lua.globals().get::<mlua::Table>("coroutine")?;
    coroutine.set("yield", task_functions.yield_)?;
    coroutine.set("wrap", wrap)?;
    coroutine.set("resume", resume)?;
    coroutine.set("close", task_functions.cancel)?;

    Ok(())
}
