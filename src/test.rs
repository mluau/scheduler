use std::time::Duration;
use tokio::time::Instant;

macro_rules! create_test {
    ($name:ident, $path:expr) => {
        #[tokio::test(flavor = "multi_thread")]
        async fn $name() {
            let lua = mlua::Lua::new();
            let task_functions = crate::functions::Functions::new(&lua).unwrap();

            let task = lua.create_table().unwrap();
            task.set("spawn", task_functions.spawn).unwrap();
            task.set("defer", task_functions.defer).unwrap();
            task.set("cancel", task_functions.cancel).unwrap();
            task.set(
                "wait",
                lua.create_async_function(|_, secs: Option<f64>| async move {
                    let now = Instant::now();
                    tokio::time::sleep(Duration::from_secs_f64(secs.unwrap_or_default())).await;
                    Ok((Instant::now() - now).as_secs_f64())
                })
                .unwrap(),
            )
            .unwrap();
            task.set(
                "delay",
                lua.create_async_function(|lua, (secs, func): (f64, mlua::Function)| async move {
                    tokio::time::sleep(Duration::from_secs_f64(secs)).await;

                    crate::spawn_local(
                        &lua,
                        lua.create_thread(func).unwrap(),
                        crate::SpawnProt::Spawn,
                        (),
                    )
                    .await
                    .expect("Failed to spawn thread");

                    Ok(())
                })
                .unwrap(),
            )
            .unwrap();

            lua.globals().set("task", task).unwrap();

            crate::setup_scheduler(&lua);

            let chunk = lua.load(include_str!($path));

            crate::spawn_local(
                &lua,
                lua.create_thread(
                    chunk
                        .into_function()
                        .expect("Failed to turn chunk into function"),
                )
                .expect("Failed to turn function into thread"),
                crate::SpawnProt::Spawn,
                (),
            )
            .await
            .expect("Failed to spawn thread");

            assert!(crate::await_scheduler(&lua).await.errors.is_empty());
        }
    };
}

create_test!(spawn, "../tests/spawn.luau");
create_test!(defer, "../tests/defer.luau");
