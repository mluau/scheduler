use clap::Parser;
use mlua_scheduler::XRc;
use mlua_scheduler_ext::traits::LuaSchedulerExt;
use smol::fs;
use std::{env::consts::OS, path::PathBuf, sync::atomic::AtomicU64, time::Duration};

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

async fn spawn_script(lua: mlua::Lua, path: PathBuf) -> mlua::Result<()> {
    let f = lua
        .load(fs::read_to_string(&path).await?)
        .set_name(fs::canonicalize(&path).await?.to_string_lossy())
        .into_function()?;

    let th = lua.create_thread(f)?;
    //println!("Spawning thread: {:?}", th.to_pointer());

    mlua_scheduler::spawn_thread(lua, th.clone(), mlua::MultiValue::new());

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

        pub struct TaskMgrFeedback {
            pub limit: u64,
            pub created: AtomicU64,
        }

        impl mlua_scheduler::taskmgr::SchedulerFeedback for TaskMgrFeedback {
            fn on_thread_add(
                &self,
                _label: &str,
                _creator: &mlua::Thread,
                _thread: &mlua::Thread,
            ) -> mlua::Result<()> {
                if self
                    .created
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    >= self.limit
                {
                    return Err(mlua::Error::RuntimeError(
                        "Thread create+spawn+add limit reached".to_string(),
                    ));
                }

                Ok(())
            }

            fn on_response(
                &self,
                label: &str,
                _tm: &mlua_scheduler::taskmgr::TaskManager,
                _th: &mlua::Thread,
                result: Option<&Result<mlua::MultiValue, mlua::Error>>,
            ) {
                if let Some(Err(e)) = result {
                    eprintln!("Error [{}]: {}", label, e);
                }
            }
        }

        let task_mgr = mlua_scheduler::taskmgr::TaskManager::new(
            lua.clone(),
            mlua_scheduler::AsyncHandleMode::PollNTimes(1),
            XRc::new(TaskMgrFeedback {
                limit: 100000000,
                created: AtomicU64::new(0),
            }),
        );

        task_mgr.attach(&lua);

        let task_mgr_ref = task_mgr.clone();
        local.spawn_local(async move {
            task_mgr_ref
                .run(Duration::from_millis(1))
                .await
                .expect("Failed to run task manager");
        });

        lua.globals()
            .set("_OS", OS.to_lowercase())
            .expect("Failed to set _OS global");

        lua.globals()
            .set(
                "_TEST_SYNC_WORK",
                lua.create_async_function(|lua, n: u64| async move {
                    //let task_mgr = taskmgr::get(&lua);
                    //println!("Async work: {}", n);
                    tokio::time::sleep(std::time::Duration::from_secs(n)).await;
                    //println!("Async work done: {}", n);

                    let created_table = lua.create_table()?;
                    created_table.set("test", "test")?;

                    Ok(created_table)
                })
                .expect("Failed to create async function"),
            )
            .expect("Failed to set _OS global");

        lua.globals()
            .set(
                "_TEST_ASYNC_WORK",
                lua.create_scheduler_async_function(|_, n: u64| async move {
                    tokio::time::sleep(std::time::Duration::from_secs(n)).await;
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

        mlua_scheduler::userdata::patch_coroutine_lib(&lua).expect("Failed to patch coroutine lib");

        lua.sandbox(true).expect("Sandboxed VM"); // Sandbox VM

        for path in cli.path {
            spawn_script(lua.clone(), path)
                .await
                .expect("Failed to spawn script");

            task_mgr.wait_till_done(Duration::from_millis(100)).await;
        }

        println!("Stopping task manager");

        task_mgr.stop();
        //std::process::exit(0);
    });
}
