#[tokio::test]
async fn test_spawn() {
    let lua = mlua::Lua::new();

    crate::inject_globals(&lua);
    crate::setup_scheduler(&lua);

    let chunk = lua
        .load(include_str!("../tests/spawn.luau"))
        .set_name("./tests/spawn.luau");

    crate::spawn_local(
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

    assert!(crate::await_scheduler(&lua).await.errors.is_empty());
}
