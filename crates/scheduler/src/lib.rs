pub mod heartbeat;
pub mod taskmgr;
pub mod userdata;

/// Spawns a function on the Lua runtime
pub fn spawn_thread(lua: mlua::Lua, th: mlua::Thread, args: mlua::MultiValue) {
    let task_msg = taskmgr::get(&lua);
    task_msg.add_deferred_thread(th, args);
}
