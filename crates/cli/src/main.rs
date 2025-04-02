use clap::Parser;
use mlua::prelude::*;
use mlua_scheduler::LuaSchedulerAsync;
use mlua_scheduler::XRc;
use mlua_scheduler_ext::Scheduler;
use std::cell::Cell;
use std::{env::consts::OS, path::PathBuf, time::Duration};
use tokio::fs;

fn get_default_log_path() -> PathBuf {
    std::env::var("TFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap().join("examples/bench.luau"))
}

#[derive(Debug, Parser)]
struct Cli {
    #[arg(default_value=get_default_log_path().into_os_string())]
    path: Vec<PathBuf>,
}

async fn spawn_script(lua: mlua::Lua, path: PathBuf, g: LuaTable) -> mlua::Result<()> {
    let f = lua
        .load(fs::read_to_string(&path).await?)
        .set_name(fs::canonicalize(&path).await?.to_string_lossy())
        .set_environment(g)
        .into_function()?;

    let th = lua.create_thread(f)?;
    //println!("Spawning thread: {:?}", th.to_pointer());

    let scheduler = mlua_scheduler_ext::Scheduler::get(&lua);
    let output = scheduler
        .spawn_thread_and_wait("SpawnScript", th, mlua::MultiValue::new())
        .await;

    println!("Output: {:?}", output);

    //println!("Spawned thread: {:?}", th.to_pointer());
    Ok(())
}

fn main() {
    env_logger::init();

    let cli = Cli::parse();

    println!("Running script: {:?}", cli.path);

    // Create tokio runtime and use spawn_local
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .worker_threads(10)
        .build()
        .unwrap();

    let local = tokio::task::LocalSet::new();

    local.block_on(&rt, async {
        let lua = mlua::Lua::new_with(mlua::StdLib::ALL_SAFE, mlua::LuaOptions::default())
            .expect("Failed to create Lua");

        #[cfg(feature = "ncg")]
        lua.enable_jit(true);

        let compiler = mlua::Compiler::new().set_optimization_level(2);

        lua.set_compiler(compiler);

        let thread_tracker = mlua_scheduler_ext::feedbacks::ThreadTracker::new();

        pub struct TaskPrintError {}

        impl mlua_scheduler::taskmgr::SchedulerFeedback for TaskPrintError {
            fn on_thread_add(
                &self,
                _label: &str,
                _creator: &mlua::Thread,
                _thread: &mlua::Thread,
            ) -> mlua::Result<()> {
                Ok(())
            }

            fn on_response(
                &self,
                _label: &str,
                _tm: &mlua_scheduler::TaskManager,
                _th: &mlua::Thread,
                result: mlua::Result<mlua::MultiValue>,
            ) {
                match result {
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("Error: {}", e);
                    }
                }
            }
        }

        lua.set_app_data(thread_tracker.clone());

        let task_mgr = mlua_scheduler::taskmgr::TaskManager::new(
            lua.clone(),
            XRc::new(mlua_scheduler_ext::feedbacks::ChainFeedback::new(
                thread_tracker,
                TaskPrintError {},
            )),
            std::time::Duration::from_millis(1),
        );

        let scheduler = mlua_scheduler_ext::Scheduler::new(task_mgr.clone());

        scheduler.attach();

        lua.globals()
            .set("_OS", OS.to_lowercase())
            .expect("Failed to set _OS global");

        lua.globals()
            .set(
                "_TEST_ASYNC_WORK",
                lua.create_scheduler_async_function(|lua, n: u64| async move {
                    tokio::time::sleep(std::time::Duration::from_secs(n)).await;
                    lua.create_table()
                })
                .expect("Failed to create async function"),
            )
            .expect("Failed to set _OS global");

        /*let count_v = Cell::new(0);

        let mut max_threads = i64::MAX;

        if let Ok(v) = std::env::var("MAX_THREADS") {
            if let Ok(v) = v.parse::<i64>() {
                if v > 0 && v < max_threads {
                    // Override the max threads
                    max_threads = v;
                }
            }
        }

        lua.set_thread_event_callback(move |lua, value| {
            match value {
                LuaValue::Thread(_) => count_v.set(count_v.get() + 1),
                _ => count_v.set(count_v.get() - 1),
            };

            if count_v.get() > max_threads {
                // Prevent runaway threads
                println!(
                    "Warning: Thread count exceeded limit: {} (max: {}). The exact way the thread will error is UNDEFINED BEHAVIOR but is guaranteed to be safe from a memory standpoint",
                    count_v.get(),
                    max_threads
                );

                let err = mlua::Error::RuntimeError(
                    "Too many threads created, possible runaway detected".to_string(),
                );

                // Push the error to the scheduler
                if let LuaValue::Thread(th) = value {
                    let scheduler = Scheduler::get(lua);
                    if let Err(e) = scheduler
                    .feedback()
                    .on_thread_add("AddFailedThread", &lua.current_thread(), &th) {
                        println!("Failed to track thread due to: {}", e);
                    }           

                    scheduler
                    .feedback()
                    .on_response("AddFailedThread", &scheduler, &th, Err(err.clone()));               
                }
                
                Err(err)
            } else {
                Ok(())
            }
        });*/

        lua.globals()
            .set(
                "_ERROR",
                lua.create_scheduler_async_function(|_lua, n: i32| async move {
                    if n % 10 == 3 {
                        return Ok(format!("Y{}", n));
                    }
                    Err(mlua::Error::external(n.to_string()))
                })
                .expect("Failed to create async function"),
            )
            .expect("Failed to set _OS global");

        let scheduler_lib =
            mlua_scheduler::userdata::scheduler_lib(&lua).expect("Failed to create scheduler lib");

        lua.globals()
            .set("scheduler", scheduler_lib.clone())
            .expect("Failed to set scheduler global");

        lua.globals()
            .set(
                "task",
                mlua_scheduler::userdata::task_lib(&lua, scheduler_lib)
                    .expect("Failed to create table"),
            )
            .expect("Failed to set task global");

        lua.sandbox(true).expect("Sandboxed VM"); // Sandbox VM

        // Setup the global table using a metatable
        //
        // SAFETY: This works because the global table will not change in the VM
        let global_mt = lua.create_table().expect("Failed to create table");
        let global_tab = lua.create_table().expect("Failed to create table");

        // Proxy reads to globals if key is in globals, otherwise to the table
        global_mt
            .set("__index", lua.globals())
            .expect("Failed to set __index");
        global_tab
            .set("_G", global_tab.clone())
            .expect("Failed to set _G");

        // Provies writes
        // Forward to _G if key is in globals, otherwise to the table
        let globals_ref = lua.globals();
        global_mt
            .set(
                "__newindex",
                lua.create_function(
                    move |_lua, (tab, key, value): (LuaTable, LuaValue, LuaValue)| {
                        let v = globals_ref.get::<LuaValue>(key.clone())?;

                        if !v.is_nil() {
                            globals_ref.set(key, value)
                        } else {
                            tab.raw_set(key, value)
                        }
                    },
                )
                .expect("Failed to create function"),
            )
            .expect("Failed to set __newindex");

        // Set __index on global_tab to point to _G
        global_tab.set_metatable(Some(global_mt));

        for path in cli.path {
            spawn_script(lua.clone(), path, global_tab.clone())
                .await
                .expect("Failed to spawn script");

            task_mgr.wait_till_done(Duration::from_millis(1000)).await;
        }

        println!("Stopping task manager");

        task_mgr.stop();
        //std::process::exit(0);
    });
}
