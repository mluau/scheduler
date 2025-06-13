use mlua::prelude::*;
use crate::{MaybeSync, TaskManager};

pub fn create_async_lua_function<A, F, R, FR>(lua: &Lua, func: F) -> LuaResult<LuaFunction>
where
    A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
    R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static,
{
    lua
    .create_function(move |lua, args| {
        #[cfg(feature = "v2_taskmgr")] {
            let func_ref = func.clone();

            let weak_lua = lua.weak();

            let fut = async move {
                let Some(lua) = weak_lua.try_upgrade() else {
                    return Err(LuaError::RuntimeError("Lua instance is no longer valid".to_string()));
                };

                match (func_ref)(lua, args).await {
                    Ok(res) => {
                        let Some(lua) = weak_lua.try_upgrade() else {
                            return Err(LuaError::RuntimeError("Lua instance is no longer valid".to_string()));
                        };

                        res.into_lua_multi(&lua)
                    },
                    Err(e) => Err(e)
                }
            };

            let taskmgr = lua
                .app_data_ref::<TaskManager>()
                .expect("Failed to get task manager")
                .clone();

            taskmgr.inner.push_event(crate::taskmgr_v2::SchedulerEvent::AddAsync {
                thread: lua.current_thread(),
                fut: Box::pin(fut),
            });
        }

        #[cfg(not(feature = "v2_taskmgr"))]
        {
            let func_ref = func.clone();

            async fn exec_fut<
                A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
                F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
                FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static,
                R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
            >(lua_fut: &mlua::WeakLua, args: LuaMultiValue, func_ref: F) -> LuaResult<LuaMultiValue> {
                //println!("ExecFut");
                let Some(lua) = lua_fut.try_upgrade() else {
                    return Err(LuaError::RuntimeError("Lua instance is no longer valid".to_string()));
                };

                let args = A::from_lua_multi(args, &lua)?;
                let fut = (func_ref)(lua, args);
                let res = fut.await;

                match res {
                    Ok(res) => {
                        let Some(lua) = lua_fut.try_upgrade() else {
                            return Err(LuaError::RuntimeError("Lua instance is no longer valid".to_string()));
                        };        

                        res.into_lua_multi(&lua)
                    },
                    Err(err) => Err(err),
                }
            }

            async fn fut<
                A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
                F: Fn(Lua, A) -> FR + mlua::MaybeSend + MaybeSync + Clone + 'static,
                FR: futures_util::Future<Output = LuaResult<R>> + mlua::MaybeSend + MaybeSync + 'static,
                R: mlua::IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
            >(taskmgr: TaskManager, lua_fut: mlua::WeakLua, th: LuaThread, args: LuaMultiValue, func_ref: F) {
                //println!("fut");
                let res = exec_fut(&lua_fut, args, func_ref).await;

                // Ensure Lua VM is still active right now
                if lua_fut.try_upgrade().is_none() {
                    return;
                };

                match res {
                    Ok(res) => {
                        taskmgr.inner.decr_async();

                        let result = th.resume(res);

                        taskmgr
                            .feedback()
                            .on_response("AsyncThread", &th, result);
                    }
                    Err(err) => {
                        taskmgr.inner.decr_async();

                        let result = th.resume_error::<LuaMultiValue>(err.to_string());

                        taskmgr
                            .feedback()
                            .on_response("AsyncThread.Resume", &th, result);
                    }
                }
            }

            let taskmgr = lua
                .app_data_ref::<TaskManager>()
                .expect("Failed to get task manager")
                .clone();

            //println!("acquired taskmgr");

            taskmgr.inner.incr_async();

            let inner = taskmgr.inner.clone();
            let mut async_executor = inner.async_task_executor.borrow_mut();

            let fut = fut(taskmgr, lua.weak(), lua.current_thread(), args, func_ref);
            //println!("fut made");
            
            #[cfg(feature = "send")]
            async_executor.spawn(fut);
            #[cfg(not(feature = "send"))]
            async_executor.spawn_local(fut);   
        }

        lua.yield_with(())?;
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
}

#[cfg(feature = "v2_taskmgr")]
pub trait LuaSchedulerAsyncUserData<T> {
    fn add_scheduler_async_method<M, A, MR, R>(&mut self, name: impl ToString, method: M)
    where
        T: 'static,
        M: Fn(Lua, mlua::UserDataRef<T>, A) -> MR + mlua::MaybeSend + MaybeSync + Clone + 'static,
        A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        MR: std::future::Future<Output = mlua::Result<R>> + mlua::MaybeSend + MaybeSync + 'static,
        R: IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static;

    fn add_scheduler_async_method_mut<M, A, MR, R>(&mut self, name: impl ToString, method: M)
    where
        T: 'static,
        M: Fn(Lua, mlua::UserDataRefMut<T>, A) -> MR + mlua::MaybeSend + MaybeSync + Clone + 'static,
        A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        MR: std::future::Future<Output = mlua::Result<R>> + mlua::MaybeSend + MaybeSync + 'static,
        R: IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static;
}

#[cfg(feature = "v2_taskmgr")]
impl<T> LuaSchedulerAsyncUserData<T> for mlua::UserDataRegistry<T> {
    fn add_scheduler_async_method<M, A, MR, R>(&mut self, name: impl ToString, method: M)
    where
        T: 'static,
        M: Fn(Lua, mlua::UserDataRef<T>, A) -> MR + mlua::MaybeSend + MaybeSync + Clone + 'static,
        A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        MR: std::future::Future<Output = mlua::Result<R>> + mlua::MaybeSend + MaybeSync + 'static,
        R: IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    {
        self.add_function(name, move |lua, (this, args): (mlua::UserDataRef<T>, A)| {
            let func_ref = method.clone();

            let weak_lua = lua.weak();

            let fut = async move {
                let Some(lua) = weak_lua.try_upgrade() else {
                    return Err(LuaError::RuntimeError("Lua instance is no longer valid".to_string()));
                };

                match (func_ref)(lua, this, args).await {
                    Ok(res) => {
                        let Some(lua) = weak_lua.try_upgrade() else {
                            return Err(LuaError::RuntimeError("Lua instance is no longer valid".to_string()));
                        };

                        res.into_lua_multi(&lua)
                    },
                    Err(e) => Err(e)
                }
            };

            let taskmgr = lua
                .app_data_ref::<TaskManager>()
                .expect("Failed to get task manager")
                .clone();

            taskmgr.inner.push_event(crate::taskmgr_v2::SchedulerEvent::AddAsync {
                thread: lua.current_thread(),
                fut: Box::pin(fut),
            });

            lua.yield_with(())?;
            Ok(())
        });
    }

    fn add_scheduler_async_method_mut<M, A, MR, R>(&mut self, name: impl ToString, method: M)
    where
        T: 'static,
        M: Fn(Lua, mlua::UserDataRefMut<T>, A) -> MR + mlua::MaybeSend + MaybeSync + Clone + 'static,
        A: FromLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
        MR: std::future::Future<Output = mlua::Result<R>> + mlua::MaybeSend + MaybeSync + 'static,
        R: IntoLuaMulti + mlua::MaybeSend + MaybeSync + 'static,
    {
        self.add_function(name, move |lua, (this, args): (mlua::UserDataRefMut<T>, A)| {
            let func_ref = method.clone();

            let weak_lua = lua.weak();

            let fut = async move {
                let Some(lua) = weak_lua.try_upgrade() else {
                    return Err(LuaError::RuntimeError("Lua instance is no longer valid".to_string()));
                };

                match (func_ref)(lua, this, args).await {
                    Ok(res) => {
                        let Some(lua) = weak_lua.try_upgrade() else {
                            return Err(LuaError::RuntimeError("Lua instance is no longer valid".to_string()));
                        };

                        res.into_lua_multi(&lua)
                    },
                    Err(e) => Err(e)
                }
            };

            let taskmgr = lua
                .app_data_ref::<TaskManager>()
                .expect("Failed to get task manager")
                .clone();

            taskmgr.inner.push_event(crate::taskmgr_v2::SchedulerEvent::AddAsync {
                thread: lua.current_thread(),
                fut: Box::pin(fut),
            });

            lua.yield_with(())?;
            Ok(())
        });
    }
}
