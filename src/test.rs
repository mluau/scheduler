#[tokio::test]
async fn test_spawn() {
    let lua = mlua::Lua::new();

    crate::inject_globals(&lua);
    crate::setup_scheduler(&lua);

    let chunk = lua
        .load(include_str!("../tests/spawn.luau"))
        .set_name("./tests/spawn.luau");

    if let Err(err) = chunk.exec_async().await {
        eprintln!("{err}");
        panic!();
    };

    assert!(crate::await_scheduler(&lua).await.errors.is_empty());
}
