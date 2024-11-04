pub fn is_poll_pending(value: &mlua::Value) -> bool {
    value
        .as_light_userdata()
        .is_some_and(|l| l == mlua::Lua::poll_pending())
}
