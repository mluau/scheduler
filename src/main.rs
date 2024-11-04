use std::time::Duration;
use tokio::{fs, task::JoinHandle};

struct ThreadHandle {
    tokio: Option<JoinHandle<()>>,
}

struct JoinHandles(Vec<ThreadHandle>);

async fn lua_wait(_lua: mlua::Lua, amount: f64) -> mlua::Result<()> {
    tokio::time::sleep(Duration::from_secs_f64(amount)).await;

    Ok(())
}

fn is_poll_pending(value: &mlua::Value) -> bool {
    value
        .as_light_userdata()
        .is_some_and(|l| l == mlua::Lua::poll_pending())
}

async fn lua_spawn(
    lua: mlua::Lua,
    (func, args): (mlua::Function, mlua::MultiValue),
) -> mlua::Result<usize> {
    let mut join_handles = lua.app_data_mut::<JoinHandles>().unwrap();
    let thread = lua.create_thread(func).unwrap();

    match thread.resume::<mlua::MultiValue>(args.clone()) {
        Ok(v) => {
            if v.get(0).is_some_and(is_poll_pending) {
                join_handles.0.push(ThreadHandle {
                    tokio: Some(tokio::spawn(async move {
                        let stream = thread.into_async::<()>(args);

                        stream.await.expect("Failed to run spawned thread");
                    })),
                });
            } else {
                join_handles.0.push(ThreadHandle { tokio: None });
            }
        }
        Err(err) => {
            join_handles.0.push(ThreadHandle { tokio: None });

            panic!("{err}")
        }
    };

    Ok(join_handles.0.len())
}

async fn lua_cancel(lua: mlua::Lua, thread_id: usize) -> mlua::Result<()> {
    let mut join_handles = lua.app_data_mut::<JoinHandles>().unwrap();
    let handle = join_handles
        .0
        .get_mut(thread_id - 1)
        .expect("Failed to get thread");

    if let Some(tokio_handle) = handle.tokio.take() {
        tokio_handle.abort();
    }

    Ok(())
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let lua = mlua::Lua::new();
    let globals = lua.globals();

    let wait_fn = lua
        .create_async_function(lua_wait)
        .expect("Failed to create wait function");

    let spawn_fn = lua
        .create_async_function(lua_spawn)
        .expect("Failed to create spawn function");

    let cancel_fn = lua
        .create_async_function(lua_cancel)
        .expect("Failed to create cancel function");

    globals
        .set("wait", wait_fn)
        .expect("Failed to set global 'wait'");

    globals
        .set("spawn", spawn_fn)
        .expect("Failed to set global 'spawn'");

    globals
        .set("cancel", cancel_fn)
        .expect("Failed to set global 'cancel'");

    lua.set_app_data(JoinHandles(Vec::new()));

    let chunk = lua.load(
        fs::read_to_string("init.luau")
            .await
            .expect("Failed to read init.luau"),
    );

    if let Err(err) = chunk.exec_async().await {
        println!("{err}")
    };

    let join_handles = lua.remove_app_data::<JoinHandles>().unwrap();

    for mut handle in join_handles.0 {
        if let Some(tokio_handle) = handle.tokio.take() {
            tokio_handle.await.expect("Spawned thread failed")
        }
    }
}
