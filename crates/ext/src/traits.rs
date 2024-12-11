use mlua::prelude::*;

/**
    Trait for any struct that can be turned into an [`LuaThread`]
    and passed to the scheduler, implemented for the following types:

    - Lua threads ([`LuaThread`])
    - Lua functions ([`LuaFunction`])
    - Lua chunks ([`LuaChunk`])
*/
pub trait IntoLuaThread {
    /**
        Converts the value into a Lua thread.

        # Errors

        Errors when out of memory.
    */
    fn into_lua_thread(self, lua: &Lua) -> LuaResult<LuaThread>;
}

impl IntoLuaThread for LuaThread {
    fn into_lua_thread(self, _: &Lua) -> LuaResult<LuaThread> {
        Ok(self)
    }
}

impl IntoLuaThread for LuaFunction {
    fn into_lua_thread(self, lua: &Lua) -> LuaResult<LuaThread> {
        lua.create_thread(self)
    }
}

impl IntoLuaThread for LuaChunk<'_> {
    fn into_lua_thread(self, lua: &Lua) -> LuaResult<LuaThread> {
        lua.create_thread(self.into_function()?)
    }
}

impl<T> IntoLuaThread for &T
where
    T: IntoLuaThread + Clone,
{
    fn into_lua_thread(self, lua: &Lua) -> LuaResult<LuaThread> {
        self.clone().into_lua_thread(lua)
    }
}
