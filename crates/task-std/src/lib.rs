use std::time::{Duration, Instant};

pub fn inject_globals(lua: &mlua::Lua) -> mlua::Result<()> {
    let task_functions = mlua_scheduler::functions::Functions::new(&lua)?;

    let task = lua.create_table()?;
    task.set("spawn", task_functions.spawn)?;
    task.set("defer", task_functions.defer)?;
    task.set("cancel", task_functions.cancel)?;
    task.set(
        "wait",
        lua.create_async_function(|_, secs: Option<f64>| async move {
            let now = Instant::now();
            smol::Timer::after(Duration::from_secs_f64(secs.unwrap_or_default())).await;
            Ok((Instant::now() - now).as_secs_f64())
        })?,
    )?;
    task.set(
        "delay",
        lua.create_async_function(
            |lua, (secs, func, args): (f64, mlua::Function, mlua::MultiValue)| async move {
                mlua_scheduler::spawn_future(&lua.clone(), async move {
                    smol::Timer::after(Duration::from_secs_f64(secs)).await;

                    mlua_scheduler::spawn_thread(
                        &lua,
                        lua.create_thread(func).unwrap(),
                        mlua_scheduler::SpawnProt::Spawn,
                        args,
                    )
                    .expect("Failed to spawn thread");
                })
                .detach();

                Ok(())
            },
        )?,
    )?;

    lua.globals().set("task", task)?;

    let coroutine = lua.globals().get::<mlua::Table>("coroutine")?;
    coroutine.set("yield", task_functions.yield_)?;
    coroutine.set(
        "wrap",
        lua.create_function(|lua, func: mlua::Function| {
            let thread = lua.create_thread(func)?;

            lua.create_function(move |lua: &mlua::Lua, args: mlua::MultiValue| {
                mlua_scheduler::spawn_thread(
                    lua,
                    thread.clone(),
                    mlua_scheduler::SpawnProt::Spawn,
                    args,
                )
            })
        })?,
    )?;

    Ok(())
}
