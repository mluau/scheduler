use std::sync::atomic::Ordering;

use mlua::prelude::*;

use crate::taskmgr::ErrorUserdataValue;

#[cfg(feature = "send")]
pub trait MaybeSync: Sync {}
#[cfg(feature = "send")]
impl<T: Sync> MaybeSync for T {}
#[cfg(not(feature = "send"))]
pub trait MaybeSync {}
#[cfg(not(feature = "send"))]
impl<T> MaybeSync for T {}

/// An async callback
#[cfg(not(feature = "send"))]
#[async_trait::async_trait(?Send)]
pub trait AsyncCallback {
    async fn call(&mut self, lua: Lua, args: LuaMultiValue) -> LuaResult<mlua::MultiValue>;
    fn clone_box(&self) -> Box<dyn AsyncCallback>;
}

/// An async callback
#[cfg(feature = "send")]
pub trait AsyncCallback: Send + Sync {
    fn call<'life0, 'async_trait>(
        &'life0 mut self,
        lua: Lua,
        args: LuaMultiValue,
    ) -> ::core::pin::Pin<
        Box<
            dyn ::core::future::Future<Output = LuaResult<mlua::MultiValue>>
                + ::core::marker::Send
                + ::core::marker::Sync
                // We can't use direct async trait here as we need the future to be Send+Sync
                + 'async_trait,
        >,
    >
    where
        'life0: 'async_trait,
        Self: 'async_trait,
        Self: Send + Sync;

    fn clone_box(&self) -> Box<dyn AsyncCallback>;
}

/// An async callback wrapper that can be used as userdata
pub struct AsyncCallbackData {
    pub callback: Box<dyn AsyncCallback>,
}

impl std::fmt::Debug for AsyncCallbackData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AsyncCallbackData")
    }
}

impl Clone for AsyncCallbackData {
    fn clone(&self) -> Self {
        AsyncCallbackData {
            callback: self.callback.clone_box(),
        }
    }
}

// All async callback data's are userdata
impl LuaUserData for AsyncCallbackData {}

impl AsyncCallbackData {
    /// Calls the async callback
    pub async fn call(&mut self, lua: Lua, args: LuaMultiValue) -> LuaResult<mlua::MultiValue> {
        self.callback.call(lua, args).await
    }

    /// Creates a lua function that can be called to call the async callback
    pub fn create_lua_function(self, lua: &Lua) -> LuaResult<LuaFunction> {
        let func = lua
            .load(
                r#"
local luacall = ...

function callback(...)
    luacall(coroutine.running(), ...)
    return coroutine.yield()
end

return callback
            "#,
            )
            .call::<LuaFunction>(lua.create_function(
                move |lua, (th, args): (LuaThread, LuaMultiValue)| {
                    let self_ref = self.clone();
                    let mut callback = self_ref.callback;
                    let lua_fut = lua.clone();
                    let fut = async move { callback.call(lua_fut, args).await };

                    let lua = lua.clone();

                    let taskmgr = super::taskmgr::get(&lua);

                    #[cfg(feature = "send")]
                    taskmgr.inner.pending_asyncs.fetch_add(1, Ordering::AcqRel);
                    #[cfg(not(feature = "send"))]
                    taskmgr.inner.pending_asyncs.fetch_add(1, Ordering::Relaxed);

                    let inner = taskmgr.inner.clone();
                    let mut async_executor = inner.async_task_executor.borrow_mut();

                    let fut = async move {
                        let res = fut.await;

                        taskmgr.inner.feedback.on_response(
                            "AsyncThread",
                            &taskmgr,
                            &th,
                            Some(&res),
                        );

                        match res {
                            Ok(res) => {
                                let result =
                                    taskmgr.resume_thread("AsyncThread", th.clone(), res).await;

                                #[cfg(feature = "send")]
                                taskmgr.inner.pending_asyncs.fetch_sub(1, Ordering::AcqRel);
                                #[cfg(not(feature = "send"))]
                                taskmgr.inner.pending_asyncs.fetch_sub(1, Ordering::Relaxed);

                                taskmgr.inner.feedback.on_response(
                                    "AsyncThread.Resume",
                                    &taskmgr,
                                    &th,
                                    result.as_ref(),
                                );
                            }
                            Err(err) => {
                                let mut result = mlua::MultiValue::new();
                                result.push_back(
                                    taskmgr
                                        .inner
                                        .lua
                                        .app_data_ref::<ErrorUserdataValue>()
                                        .unwrap()
                                        .0
                                        .clone(),
                                );

                                if let Ok(v) = err.to_string().into_lua(&lua) {
                                    result.push_back(v);
                                } else {
                                    result.push_back(mlua::Value::Nil);
                                }

                                let result = taskmgr
                                    .resume_thread("AsyncThread", th.clone(), result)
                                    .await;

                                #[cfg(feature = "send")]
                                taskmgr.inner.pending_asyncs.fetch_sub(1, Ordering::AcqRel);
                                #[cfg(not(feature = "send"))]
                                taskmgr.inner.pending_asyncs.fetch_sub(1, Ordering::Relaxed);

                                taskmgr.inner.feedback.on_response(
                                    "AsyncThread.Resume",
                                    &taskmgr,
                                    &th,
                                    result.as_ref(),
                                );
                            }
                        }
                    };

                    #[cfg(not(feature = "send"))]
                    async_executor.spawn_local(fut);
                    #[cfg(feature = "send")]
                    async_executor.spawn(fut);

                    Ok(())
                },
            )?)?;

        Ok(func)
    }
}

/// Creates an async task that can then be pushed to the scheduler
pub fn create_async_task<A, F, R, FR>(func: F) -> AsyncCallbackData
where
    A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
    R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static,
{
    #[derive(Copy, Clone)]
    pub struct AsyncCallbackWrapper<A, F, R, FR> {
        func: F,
        _marker: std::marker::PhantomData<(A, R, FR)>,
    }

    #[cfg(not(feature = "send"))]
    #[async_trait::async_trait(?Send)]
    impl<A, F, R, FR> AsyncCallback for AsyncCallbackWrapper<A, F, R, FR>
    where
        A: FromLuaMulti + mlua::MaybeSend + 'static,
        F: FnMut(Lua, A) -> FR + mlua::MaybeSend + Clone + 'static,
        R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + 'static,
    {
        async fn call(&mut self, lua: Lua, args: LuaMultiValue) -> LuaResult<mlua::MultiValue> {
            let args = A::from_lua_multi(args, &lua)?;
            let fut = (self.func)(lua.clone(), args);
            let res = fut.await;

            match res {
                Ok(res) => res.into_lua_multi(&lua),
                Err(err) => Err(err),
            }
        }

        fn clone_box(&self) -> Box<dyn AsyncCallback> {
            let func = self.func.clone();
            Box::new(AsyncCallbackWrapper {
                func,
                _marker: std::marker::PhantomData,
            })
        }
    }

    #[cfg(feature = "send")]
    impl<A, F, R, FR> AsyncCallback for AsyncCallbackWrapper<A, F, R, FR>
    where
        A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        F: FnMut(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
        R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static,
    {
        fn call<'life0, 'async_trait>(
            &'life0 mut self,
            lua: Lua,
            args: LuaMultiValue,
        ) -> ::core::pin::Pin<
            Box<
                dyn ::core::future::Future<Output = LuaResult<mlua::MultiValue>>
                    + ::core::marker::Send
                    + ::core::marker::Sync
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
            // Bounds from trait:
            Self: Send + Sync,
        {
            Box::pin(async move {
                let args = A::from_lua_multi(args, &lua)?;
                let fut = (self.func)(lua.clone(), args);
                let res = fut.await;

                match res {
                    Ok(res) => res.into_lua_multi(&lua),
                    Err(err) => Err(err),
                }
            })
        }

        fn clone_box(&self) -> Box<dyn AsyncCallback> {
            let func = self.func.clone();
            Box::new(AsyncCallbackWrapper {
                func,
                _marker: std::marker::PhantomData,
            })
        }
    }

    let wrapper: AsyncCallbackWrapper<A, F, R, FR> = AsyncCallbackWrapper {
        func,
        _marker: std::marker::PhantomData,
    };

    AsyncCallbackData {
        callback: Box::new(wrapper),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_poll_once() {
        let wrapper = || async move {
            println!("Hello world");
            tokio::task::yield_now().await;
            println!("Hello world 2");
        };

        let fut = wrapper();
        futures_util::pin_mut!(fut);

        match futures_util::poll!(fut) {
            std::task::Poll::Ready(_) => println!("Ready"),
            std::task::Poll::Pending => println!("Pending"),
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        tokio::task::yield_now().await;
    }

    #[test]
    fn test_async_task_create() {
        let _lua = Lua::new();
        let task = create_async_task(|lua, n| async move {
            println!("Async work: {}", n);
            tokio::time::sleep(std::time::Duration::from_secs(n)).await;
            println!("Async work done: {}", n);

            let created_table = lua.create_table()?;
            created_table.set("test", "test")?;

            created_table.into_lua_multi(&lua)
        });

        fn a(t: AsyncCallbackData) -> AsyncCallbackData {
            t
        }

        a(task);
    }
}
