use std::time::Duration;

#[cfg(test)]
mod test;

pub mod functions;
pub(crate) mod util;

pub(crate) struct ThreadHandle {
    thread: mlua::Thread,
}

#[derive(Default)]
pub struct Scheduler {
    pub(crate) handles: Vec<ThreadHandle>,
    pub errors: Vec<mlua::Error>,
}

pub fn setup_scheduler(lua: &mlua::Lua) {
    lua.set_app_data(Scheduler::default());
}

pub fn spawn_local<A: mlua::IntoLuaMulti>(
    lua: &mlua::Lua,
    thread: mlua::Thread,
    args: A,
) -> mlua::Result<mlua::Thread> {
    let thread_inner = thread.clone();
    let args = args.into_lua_multi(lua)?;

    {
        let mut scheduler = lua.app_data_mut::<Scheduler>().unwrap();
        scheduler.handles.push(ThreadHandle {
            thread: thread.clone(),
        });
    }

    match thread.resume::<mlua::MultiValue>(args.clone()) {
        Ok(v) => {
            if v.get(0).is_some_and(util::is_poll_pending) {
                let lua_inner = lua.clone();
                tokio::spawn(async move {
                    match thread_inner.status() {
                        mlua::ThreadStatus::Resumable => {
                            let stream = thread_inner.into_async::<()>(args);

                            if let Err(err) = stream.await {
                                eprintln!("{err}");

                                let mut scheduler = lua_inner.app_data_mut::<Scheduler>().unwrap();
                                scheduler.errors.push(err.clone());
                            };
                        }
                        _ => {}
                    }
                });
            }
        }
        Err(err) => {
            let mut scheduler = lua.app_data_mut::<Scheduler>().unwrap();
            scheduler.errors.push(err.clone());

            eprintln!("{err}");
        }
    };

    Ok(thread)
}

pub async fn await_scheduler(lua: &mlua::Lua) -> Scheduler {
    tokio::time::sleep(Duration::ZERO).await;

    'main: loop {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();

        for handle in &scheduler.handles {
            match handle.thread.status() {
                mlua::ThreadStatus::Running => {
                    continue 'main;
                }
                _ => {}
            }
        }

        drop(scheduler);

        tokio::task::yield_now().await;

        break 'main;
    }

    lua.remove_app_data::<Scheduler>().unwrap()
}

pub fn inject_globals(lua: &mlua::Lua) {
    let globals = lua.globals();
    let task_functions = functions::Functions::new(&lua).expect("Failed to create task library");

    globals
        .set(
            "task",
            task_functions
                .into_dictionary(&lua)
                .expect("Failed to turn task library into lua dictionary"),
        )
        .expect("Failed to set task library");
}
