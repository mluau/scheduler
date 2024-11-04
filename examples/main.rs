use tokio::fs;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let lua = mlua::Lua::new();

    mlua_scheduler::inject_globals(&lua);
    mlua_scheduler::setup_scheduler(&lua);

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

    mlua_scheduler::await_scheduler(&lua).await;
}
