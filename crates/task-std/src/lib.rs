use std::time::Duration;
use tokio::time::Instant;

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
            tokio::time::sleep(Duration::from_secs_f64(secs.unwrap_or_default())).await;
            Ok((Instant::now() - now).as_secs_f64())
        })?,
    )?;
    task.set(
        "delay",
        lua.create_async_function(
            |lua, (secs, func, args): (f64, mlua::Function, mlua::MultiValue)| async move {
                tokio::spawn(async move {
                    let rust_func = lua
                        .create_async_function(
                            |_, (secs, func, args): (f64, mlua::Function, mlua::MultiValue)| async move {
                                tokio::time::sleep(Duration::from_secs_f64(secs)).await;
                               
                                func.call_async::<()>(args).await
                            },
                        )
                        .unwrap();

                    mlua_scheduler::spawn_local(
                        &lua,
                        lua.create_thread(rust_func).unwrap(),
                        mlua_scheduler::SpawnProt::Spawn,
                        (secs, func, args),
                    )
                    .await
                    .expect("Failed to spawn thread");
                });

                Ok(())
            },
        )?,
    )?;

    lua.globals().set("task", task)?;

    Ok(())
}
