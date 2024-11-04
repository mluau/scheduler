use tokio::fs;

const PATH: &str = "examples/init.luau";

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let lua = mlua::Lua::new();

    mlua_scheduler::inject_globals(&lua);
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
        (),
    )
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
