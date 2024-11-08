use smol::Task;
use std::future::Future;

use crate::SpawnProt;

pub trait LuaSchedulerMethods {
    fn spawn_future<T>(&self, future: impl Future<Output = T> + Send + 'static) -> Task<T>
    where
        T: Send + 'static;

    fn yield_thread(
        &self,
        thread: mlua::Thread,
    ) -> impl std::future::Future<Output = mlua::Result<mlua::MultiValue>> + Send;

    fn spawn_thread<A: mlua::IntoLuaMulti>(
        &self,
        thread: mlua::Thread,
        prot: SpawnProt,
        args: A,
    ) -> mlua::Result<mlua::Thread>;

    fn await_thread(
        &self,
        thread: mlua::Thread,
    ) -> impl std::future::Future<Output = mlua::Result<mlua::MultiValue>> + Send;

    fn cancel_thread(
        &self,
        thread: mlua::Thread,
    ) -> impl std::future::Future<Output = mlua::Result<()>> + Send;
}

impl LuaSchedulerMethods for mlua::Lua {
    fn spawn_future<T>(&self, future: impl Future<Output = T> + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
    {
        crate::spawn_future(self, future)
    }

    async fn yield_thread(&self, thread: mlua::Thread) -> mlua::Result<mlua::MultiValue> {
        crate::yield_thread(self, thread).await
    }

    fn spawn_thread<A: mlua::IntoLuaMulti>(
        &self,
        thread: mlua::Thread,
        prot: SpawnProt,
        args: A,
    ) -> mlua::Result<mlua::Thread> {
        crate::spawn_thread(self, thread, prot, args)
    }

    async fn await_thread(&self, thread: mlua::Thread) -> mlua::Result<mlua::MultiValue> {
        crate::await_thread(self, thread).await
    }

    async fn cancel_thread(&self, thread: mlua::Thread) -> mlua::Result<()> {
        crate::cancel_thread(self, thread).await
    }
}
