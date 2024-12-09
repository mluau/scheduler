use mlua::prelude::*;

/// Patches the coroutine library to both work with the scheduler properly, but also to be more sane without deadlocking
pub fn patch_coroutine_lib(lua: &Lua) -> LuaResult<()> {
    let coroutine = lua.globals().get::<LuaTable>("coroutine")?;

    coroutine.set(
        "resume",
        lua.create_async_function(|lua, (th, args): (LuaThread, LuaMultiValue)| async move {
            let taskmgr = super::taskmgr::get(&lua);
            let res = taskmgr
                .resume_thread("CoroutineResume", th.clone(), args)
                .await;

            taskmgr
                .inner
                .feedback
                .on_response("CoroutineResume", &taskmgr, &th, &res);

            match res {
                Ok(res) => (true, res).into_lua_multi(&lua),
                Err(err) => {
                    // Error, return false and error message
                    (false, err.to_string()).into_lua_multi(&lua)
                }
            }
        })?,
    )?;

    Ok(())
}

pub fn table(lua: &Lua) -> LuaResult<LuaTable> {
    let table = lua
        .load(
            r#"
local table = {}

local function defer<T...>(task: Task<T...>, ...: T...): thread
    return table.__addDeferred(task, ...)
end

local function delay<T...>(time: number, task: Task<T...>, ...: T...): thread
    return table.__addWaitingWithArgs(task, time, ...)
end

local function desynchronize(...)
    return
end

local function synchronize(...)
    return
end

local function wait(time: number?): number
    if time == nil then
        time = 0
    end
    table.__addWaiting(coroutine.running(), time)
    return coroutine.yield()
end

local function cancel(thread: thread): ()
	coroutine.close(thread)
end

table.defer = defer
table.delay = delay
table.desynchronize = desynchronize
table.synchronize = synchronize
table.wait = wait
table.cancel = cancel

return table
        "#,
        )
        .set_environment(lua.globals())
        .call::<LuaTable>(())?;

    table.set(
        "__addWaiting",
        lua.create_function(|lua, (th, resume): (LuaThread, f64)| {
            let scheduler = super::taskmgr::get(lua);
            scheduler.add_waiting_thread(
                th,
                LuaMultiValue::new(),
                std::time::Duration::from_secs_f64(resume),
            );
            Ok(())
        })?,
    )?;

    table.set(
        "__addWaitingWithArgs",
        lua.create_function(
            |lua, (f, resume, args): (LuaEither<LuaFunction, LuaThread>, f64, LuaMultiValue)| {
                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let scheduler = super::taskmgr::get(lua);
                scheduler.add_waiting_thread(
                    th.clone(),
                    args,
                    std::time::Duration::from_secs_f64(resume),
                );
                Ok(th)
            },
        )?,
    )?;

    table.set(
        "__addDeferred",
        lua.create_function(
            |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| {
                let th = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let scheduler = super::taskmgr::get(lua);
                scheduler.add_deferred_thread(th.clone(), args);
                Ok(th)
            },
        )?,
    )?;

    table.set(
        "spawn",
        lua.create_async_function(
            |lua, (f, args): (LuaEither<LuaFunction, LuaThread>, LuaMultiValue)| async move {
                let t = match f {
                    LuaEither::Left(f) => lua.create_thread(f)?,
                    LuaEither::Right(t) => t,
                };

                let taskmgr = super::taskmgr::get(&lua);
                let result = taskmgr.resume_thread("ThreadSpawn", t.clone(), args).await;

                taskmgr
                    .inner
                    .feedback
                    .on_response("TaskResume", &taskmgr, &t, &result);

                Ok(t)
            },
        )?,
    )?;

    Ok(table)
}

/// An async callback
#[cfg(not(feature = "send"))]
#[async_trait::async_trait(?Send)]
pub trait AsyncCallback {
    async fn call(&mut self, lua: &Lua, args: LuaMultiValue) -> LuaResult<mlua::MultiValue>;
}

/// An async callback
#[cfg(feature = "send")]
#[async_trait::async_trait]
pub trait AsyncCallback {
    async fn call(&mut self, lua: &Lua, args: LuaMultiValue) -> LuaResult<mlua::MultiValue>;
}

/// Creates an async task that can then be pushed to the scheduler
pub fn create_async_task<A, F, FR>(lua: &Lua, func: F) -> Box<dyn AsyncCallback>
where
    A: FromLuaMulti + mlua::MaybeSend + 'static,
    F: Fn(Lua, A) -> FR + mlua::MaybeSend + 'static,
    FR: futures_util::Future<Output = LuaResult<mlua::MultiValue>> + mlua::MaybeSend + 'static,
{
    /*let func_wrapper = lua
            .load(
                r#"
    return function(...) end
            "#,
            )
            .set_environment(lua.globals())
            .call::<LuaTable>(())?;*/

    pub struct AsyncCallbackWrapper<A, F, FR> {
        func: F,
        _marker: std::marker::PhantomData<(A, FR)>,
    }

    #[cfg(not(feature = "send"))]
    #[async_trait::async_trait(?Send)]
    impl<A, F, FR> AsyncCallback for AsyncCallbackWrapper<A, F, FR>
    where
        A: FromLuaMulti + mlua::MaybeSend + 'static,
        F: FnMut(Lua, A) -> FR + mlua::MaybeSend + 'static,
        FR: futures_util::Future<Output = LuaResult<mlua::MultiValue>> + mlua::MaybeSend + 'static,
    {
        async fn call(&mut self, lua: &Lua, args: LuaMultiValue) -> LuaResult<mlua::MultiValue> {
            let args = A::from_lua_multi(args, lua)?;
            let fut = (self.func)(lua.clone(), args);
            fut.await
        }
    }

    #[cfg(feature = "send")]
    #[async_trait::async_trait]
    impl<A, F, FR> AsyncCallback for AsyncCallbackWrapper<A, F, FR>
    where
        A: FromLuaMulti + mlua::MaybeSend + 'static,
        F: FnMut(Lua, A) -> FR + mlua::MaybeSend + 'static,
        FR: futures_util::Future<Output = LuaResult<mlua::MultiValue>> + mlua::MaybeSend + 'static,
    {
        async fn call(&mut self, lua: &Lua, args: LuaMultiValue) -> LuaResult<mlua::MultiValue> {
            let args = A::from_lua_multi(args, lua)?;
            let fut = (self.func)(lua.clone(), args);
            fut.await
        }
    }

    let mut wrapper: AsyncCallbackWrapper<A, F, FR> = AsyncCallbackWrapper {
        func,
        _marker: std::marker::PhantomData,
    };

    wrapper.call(lua, LuaMultiValue::new());

    Box::new(wrapper)
}
