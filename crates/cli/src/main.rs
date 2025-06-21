use clap::Parser;
use mlua::prelude::*;
use mlua_scheduler::LuaSchedulerAsync;
use std::{env::consts::OS, path::PathBuf};
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

async fn spawn_script(lua: &mlua::Lua, path: PathBuf, g: LuaTable) -> mlua::Result<()> {
    let f = lua
        .load(fs::read_to_string(&path).await?)
        .set_name(fs::canonicalize(&path).await?.to_string_lossy())
        .set_environment(g)
        .into_function()?;

    let th = lua.create_thread(f)?;
    //println!("Spawning thread: {:?}", th.to_pointer());

    let scheduler = mlua_scheduler::taskmgr::get(lua);
    let output = scheduler
        .spawn_thread_and_wait(th, mlua::MultiValue::new())
        .await;

    println!("Output: {output:?}");

    //println!("Spawned thread: {:?}", th.to_pointer());
    Ok(())
}

fn main() {
    env_logger::init();

    let cli = Cli::parse();

    println!("Running script: {:?}", cli.path);

    let async_closure = async {
        let lua = mlua::Lua::new_with(mlua::StdLib::ALL_SAFE, mlua::LuaOptions::default())
            .expect("Failed to create Lua");

        #[cfg(feature = "ncg")]
        lua.enable_jit(true);

        let compiler = mlua::Compiler::new().set_optimization_level(2);

        lua.set_compiler(compiler);

        let returns_tracker = mlua_scheduler::taskmgr::ReturnTracker::new();

        let mut wildcard_sender = returns_tracker.track_wildcard_thread();

        #[cfg(feature = "send")]
        tokio::task::spawn(async move {
            while let Some((thread, result)) = wildcard_sender.recv().await {
                if let Err(e) = result {
                    eprintln!("Error in thread {e:?}: {:?}", thread.to_pointer());
                }
            }
        });

        #[cfg(not(feature = "send"))]
        tokio::task::spawn_local(async move {
            while let Some((thread, result)) = wildcard_sender.recv().await {
                if let Err(e) = result {
                    eprintln!("Error in thread {e:?}: {:?}", thread.to_pointer());
                }
            }
        });

        let task_mgr = mlua_scheduler::taskmgr::TaskManager::new(&lua, returns_tracker);

        task_mgr.attach().expect("Failed to attach task manager");
        task_mgr.run_in_task();

        lua.globals()
            .set("_OS", OS.to_lowercase())
            .expect("Failed to set _OS global");

        lua.globals()
            .set(
                "_TEST_ASYNC_WORK",
                lua.create_scheduler_async_function(|_lua, mut n: f64| async move {
                    if n < 0.0 {
                        return Err(mlua::Error::external("Negative sleep time"));
                    }
                    if n < 0.05 {
                        n = 0.10; // Minimum sleep time
                    }

                    let curr_time = std::time::Instant::now();
                    tokio::time::sleep(std::time::Duration::from_secs_f64(n)).await;
                    Ok(curr_time.elapsed().as_secs_f64())
                })
                .expect("Failed to create async function"),
            )
            .expect("Failed to set _OS global");

        lua.globals()
            .set(
                "_ERROR",
                lua.create_scheduler_async_function(|_lua, n: i32| async move {
                    if n % 10 == 3 {
                        return Ok(format!("Y{n}"));
                    }
                    Err(mlua::Error::external(n.to_string()))
                })
                .expect("Failed to create async function"),
            )
            .expect("Failed to set _OS global");

        lua.globals()
            .set(
                "_PANIC",
                lua.create_scheduler_async_function(|_lua, n: i32| async move {
                    panic!("Panic test: {n}");
                    #[allow(unreachable_code)]
                    Ok(())
                })
                .expect("Failed to create async function"),
            )
            .expect("Failed to set _OS global");

        lua.globals()
            .set(
                "task",
                mlua_scheduler::userdata::task_lib(&lua).expect("Failed to create table"),
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
            spawn_script(&lua, path, global_tab.clone())
                .await
                .expect("Failed to spawn script");

            task_mgr
                .wait_till_done()
                .await
                .expect("Failed to wait for task manager");
        }

        println!("Stopping task manager");

        task_mgr.stop();
        //std::process::exit(0);

        let weak_lua = lua.weak();
        drop(lua);

        println!("Is Lua still alive?: {}", weak_lua.try_upgrade().is_some());
    };

    #[cfg(not(feature = "send"))]
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .worker_threads(10)
            .build()
            .unwrap();

        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async_closure);
    }
    #[cfg(feature = "send")]
    {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async_closure);
    }
}
