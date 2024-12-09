use mlua::prelude::*;

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

/// An async callback wrapper that can be used as userdata
pub struct AsyncCallbackData {
    pub callback: crate::XRc<dyn AsyncCallback>,
}

// All async callback data's are userdata
impl LuaUserData for AsyncCallbackData {}

/// Creates an async task that can then be pushed to the scheduler
pub fn create_async_task<A, F, FR>(func: F) -> AsyncCallbackData
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

    let wrapper: AsyncCallbackWrapper<A, F, FR> = AsyncCallbackWrapper {
        func,
        _marker: std::marker::PhantomData,
    };

    AsyncCallbackData {
        callback: crate::XRc::new(wrapper),
    }
}
