use tokio::task::JoinHandle;

#[cfg(test)]
mod test;

pub mod functions;
pub(crate) mod util;

pub(crate) struct ThreadHandle {
    tokio: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub struct Scheduler {
    pub(crate) handles: Vec<ThreadHandle>,
    pub errors: Vec<mlua::Error>,
}

pub fn setup_scheduler(lua: &mlua::Lua) {
    lua.set_app_data(Scheduler::default());
}

pub async fn await_scheduler(lua: &mlua::Lua) -> Scheduler {
    'main: loop {
        let scheduler = lua.app_data_ref::<Scheduler>().unwrap();

        for handle in &scheduler.handles {
            if let Some(tokio_handle) = &handle.tokio {
                if tokio_handle.is_finished() {
                    continue;
                }

                continue 'main;
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
