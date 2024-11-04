use tokio::task::JoinHandle;

#[cfg(test)]
mod test;

pub mod functions;
pub(crate) mod util;

pub(crate) struct ThreadHandle {
    tokio: Option<JoinHandle<()>>,
}

pub(crate) struct JoinHandles(Vec<ThreadHandle>);

pub fn setup_scheduler(lua: &mlua::Lua) {
    lua.set_app_data(JoinHandles(Vec::new()));
}

pub async fn await_scheduler(lua: &mlua::Lua) {
    'main: loop {
        let join_handles = lua.app_data_ref::<JoinHandles>().unwrap();

        for handle in &join_handles.0 {
            if let Some(tokio_handle) = &handle.tokio {
                if tokio_handle.is_finished() {
                    continue;
                }

                continue 'main;
            }
        }

        drop(join_handles);

        tokio::task::yield_now().await;

        break 'main;
    }
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
