use crate::Scheduler;
use mlua::prelude::*;
use mlua_scheduler::r#async::MaybeSync;

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

/**
    Trait for interacting with the current [`Scheduler`].

    Provides extra methods on the [`Lua`] struct for:

    - Setting the exit code and forcibly stopping the scheduler
    - Pushing (spawning) and deferring (pushing to the back) lua threads

    To add:

    - Getting thread results
*/
pub trait LuaSchedulerExt {
    /**
        Sets the exit code of the current scheduler.

        See [`Scheduler::set_exit_code`] for more information.

        # Panics

        Panics if called outside of a running [`Scheduler`].
    */
    fn set_exit_code(&self, code: u8);

    /**
        Adds a lua thread to the **front** of the current schedulers deferred thread queue.

        See [`TaskManager::add_deferred_thread_front`] for more information.

        # Panics

        Panics if called outside of a running [`mlua_scheduler::TaskManager`].
    */
    fn push_thread_front(
        &self,
        thread: impl IntoLuaThread,
        args: impl IntoLuaMulti,
    ) -> LuaResult<mlua::Thread>;

    /**
        Adds a lua thread to the **front** of the current schedulers deferred thread queue.

        See [`TaskManager::add_deferred_thread_front`] for more information.

        # Panics

        Panics if called outside of a running [`mlua_scheduler::TaskManager`].
    */
    fn push_thread_back(
        &self,
        thread: impl IntoLuaThread,
        args: impl IntoLuaMulti,
    ) -> LuaResult<mlua::Thread>;

    /**
     * Creates a scheduler-handled async function
     *
     * Note that while `create_async_function` can still be used. Functions that need to have Lua scheduler aware
     * characteristics should use this function instead.
     *
     * # Panics
     *
     * Panics if called outside of a running [`mlua_scheduler::TaskManager`].
     */
    fn create_scheduler_async_function<A, F, R, FR>(&self, func: F) -> LuaResult<LuaFunction>
    where
        A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
        R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static;
}

impl LuaSchedulerExt for Lua {
    fn set_exit_code(&self, code: u8) {
        let scheduler = self
            .app_data_ref::<Scheduler>()
            .expect("No scheduler attached");
        scheduler.exit_with_code(code);
    }

    fn push_thread_front(
        &self,
        thread: impl IntoLuaThread,
        args: impl IntoLuaMulti,
    ) -> LuaResult<mlua::Thread> {
        let scheduler = self
            .app_data_ref::<mlua_scheduler::TaskManager>()
            .expect("No task manager attached");

        let thread = thread.into_lua_thread(self)?;
        let args = args.into_lua_multi(self)?;

        scheduler.add_deferred_thread_front(thread.clone(), args);

        Ok(thread)
    }

    fn push_thread_back(
        &self,
        thread: impl IntoLuaThread,
        args: impl IntoLuaMulti,
    ) -> LuaResult<mlua::Thread> {
        let scheduler = self
            .app_data_ref::<mlua_scheduler::TaskManager>()
            .expect("No task manager attached");

        let thread = thread.into_lua_thread(self)?;
        let args = args.into_lua_multi(self)?;

        scheduler.add_deferred_thread_back(thread.clone(), args);

        Ok(thread)
    }

    fn create_scheduler_async_function<A, F, R, FR>(&self, func: F) -> LuaResult<LuaFunction>
    where
        A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
        R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static,
    {
        let func = mlua_scheduler::r#async::create_async_task(func);

        func.create_lua_function(self)
    }
}
