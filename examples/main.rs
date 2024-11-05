use std::time::Duration;
use tokio::{fs, time::Instant};

const PATH: &str = "examples/init.luau";

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let lua = mlua::Lua::new();

    let task_functions = mlua_scheduler::functions::Functions::new(&lua).unwrap();

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

            mlua_scheduler::spawn_local(
                &lua,
                lua.create_thread(func).unwrap(),
                mlua_scheduler::SpawnProt::Spawn,
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

    mlua_scheduler::setup_scheduler(&lua);

    let chunk = lua
        .load(
            fs::read_to_string(PATH)
                .await
                .expect("Failed to read init.luau"),
        )
        .set_name(fs::canonicalize(PATH).await.unwrap().to_string_lossy());

    mlua_scheduler::spawn_local(
        &lua,
        lua.create_thread(
            chunk
                .into_function()
                .expect("Failed to turn chunk into function"),
        )
        .expect("Failed to turn function into thread"),
        mlua_scheduler::SpawnProt::Spawn,
        (),
    )
    .await
    .expect("Failed to spawn thread");

    std::process::exit(
        if mlua_scheduler::await_scheduler(&lua)
            .await
            .errors
            .is_empty()
        {
            0
        } else {
            1
        },
    )
}
