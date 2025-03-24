use mlua::prelude::*;

use crate::{MaybeSync, TaskManager};

pub fn create_async_lua_function<A, F, R, FR>(lua: &Lua, func: F) -> LuaResult<LuaFunction>
where
    A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
    R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static,
{
    let func = lua
        .load(
            r#"
local luacall = ...

local function callback(...)
    luacall(coroutine.running(), ...)
    return coroutine.yield()
end

return callback
            "#,
        )
        .set_name("__sched_yield")
        .call::<LuaFunction>(inner_async_func(lua, func))?;

    Ok(func)
}

/// Create an async lua function with custom luau code (e.g. to extract stack traces/chunk names for custom requires etc.)
///
/// This luau code will be passed the ``luacall`` function as its first argument.
pub fn create_async_lua_function_with<A, F, R, FR>(
    lua: &Lua,
    func: F,
    lua_code: &str,
    extra_args: LuaMultiValue,
) -> LuaResult<LuaFunction>
where
    A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
    R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static,
{
    let func = lua
        .load(lua_code)
        .set_name("__sched_yield")
        .call::<LuaFunction>((inner_async_func(lua, func)?, extra_args))?;

    Ok(func)
}

/// The inner async function driving create_async_lua_function
pub fn inner_async_func<A, F, R, FR>(lua: &Lua, func: F) -> LuaResult<LuaFunction>
where
    A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
    R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static,
{
    lua.create_function(move |lua, (th, args): (LuaThread, LuaMultiValue)| {
        let func_ref = func.clone();
        let lua_fut = lua.clone();
        let fut = async move {
            let args = A::from_lua_multi(args, &lua_fut)?;
            let fut = (func_ref)(lua_fut.clone(), args);
            let res = fut.await;

            match res {
                Ok(res) => res.into_lua_multi(&lua_fut),
                Err(err) => Err(err),
            }
        };

        let taskmgr = lua
            .app_data_ref::<TaskManager>()
            .expect("Failed to get task manager")
            .clone();

        {
            let current_pending = taskmgr.inner.pending_asyncs.get();
            taskmgr.inner.pending_asyncs.set(current_pending + 1);
        };

        let inner = taskmgr.inner.clone();
        let mut async_executor = inner.async_task_executor.borrow_mut();

        let fut = async move {
            let res = fut.await;

            match res {
                Ok(res) => {
                    {
                        let current_pending = taskmgr.inner.pending_asyncs.get();
                        taskmgr.inner.pending_asyncs.set(current_pending - 1);
                    }

                    let result = th.resume(res);

                    taskmgr
                        .inner
                        .feedback
                        .on_response("AsyncThread", &taskmgr, &th, result);
                }
                Err(err) => {
                    {
                        let current_pending = taskmgr.inner.pending_asyncs.get();
                        taskmgr.inner.pending_asyncs.set(current_pending - 1);
                    }

                    let result = th.resume_error::<LuaMultiValue>(err.to_string());

                    taskmgr
                        .inner
                        .feedback
                        .on_response("AsyncThread.Resume", &taskmgr, &th, result);
                }
            }
        };

        async_executor.spawn_local(fut);

        Ok(())
    })
}

pub trait LuaSchedulerAsync {
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

    fn create_scheduler_async_function_with<A, F, R, FR>(
        &self,
        func: F,
        lua_code: &str,
        extra_args: LuaMultiValue,
    ) -> LuaResult<LuaFunction>
    where
        A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
        R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static;
}

impl LuaSchedulerAsync for Lua {
    fn create_scheduler_async_function<A, F, R, FR>(&self, func: F) -> LuaResult<LuaFunction>
    where
        A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
        R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static,
    {
        create_async_lua_function(self, func)
    }

    fn create_scheduler_async_function_with<A, F, R, FR>(
        &self,
        func: F,
        lua_code: &str,
        extra_args: LuaMultiValue,
    ) -> LuaResult<LuaFunction>
    where
        A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
        R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static,
    {
        create_async_lua_function_with(self, func, lua_code, extra_args)
    }
}

/*pub trait LuaSchedulerAsyncUserData<T> {
    fn add_async_method<M, A, MR, R>(&mut self, name: impl ToString, method: M)
    where
        T: 'static,
        M: Fn(Lua, mlua::UserDataRef<T>, A) -> MR + mlua::MaybeSend + 'static,
        A: FromLuaMulti,
        MR: std::future::Future<Output = mlua::Result<R>> + mlua::MaybeSend + 'static,
        R: IntoLuaMulti;
}

impl<T> LuaSchedulerAsyncUserData<T> for mlua::UserDataRegistry<T> {
    fn add_async_method<M, A, MR, R>(&mut self, name: impl ToString, method: M)
    where
        T: 'static,
        M: Fn(Lua, mlua::UserDataRef<T>, A) -> MR + mlua::MaybeSend + 'static,
        A: FromLuaMulti,
        MR: std::future::Future<Output = mlua::Result<R>> + mlua::MaybeSend + 'static,
        R: IntoLuaMulti,
    {
        self.add_method(name, move |lua, this, args| {
            let coroutine = lua.globals().raw_get::<LuaFunction>("coroutine")?;
        });
    }
}
*/
