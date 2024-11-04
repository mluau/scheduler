use tokio::{fs, task::JoinHandle};

mod functions;
pub(crate) mod util;

pub struct ThreadHandle {
    tokio: Option<JoinHandle<()>>,
}

pub struct JoinHandles(Vec<ThreadHandle>);

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let lua = mlua::Lua::new();
    let globals = lua.globals();

    globals
        .set(
            "task",
            functions::Functions::new(&lua)
                .expect("Failed to create task library")
                .into_dictionary(&lua)
                .expect("Failed to turn task library into lua dictionary"),
        )
        .expect("Failed to set task library");

    lua.set_app_data(JoinHandles(Vec::new()));

    let chunk = lua
        .load(
            fs::read_to_string("init.luau")
                .await
                .expect("Failed to read init.luau"),
        )
        .set_name(
            fs::canonicalize("init.luau")
                .await
                .unwrap()
                .to_string_lossy(),
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
