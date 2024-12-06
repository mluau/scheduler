use clap::Parser;
use smol::fs;
use std::{env::consts::OS, path::PathBuf};

fn get_default_log_path() -> PathBuf {
    std::env::var("TFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap().join("examples/bench.luau"))
}

#[derive(Debug, Parser)]
struct Cli {
    #[arg(default_value=get_default_log_path().into_os_string())]
    path: PathBuf,
}

async fn spawn_script(lua: mlua::Lua, path: PathBuf) -> mlua::Result<()> {
    let f = lua
        .load(fs::read_to_string(&path).await?)
        .set_name(fs::canonicalize(&path).await?.to_string_lossy())
        .into_function()?;

    let th = lua.create_thread(f)?;

    mlua_scheduler::spawn_thread(lua, th, mlua::MultiValue::new());
    Ok(())
}

fn main() {
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

        let mut hb = mlua_scheduler::heartbeat::Heartbearter::new();
        let hb_recv = hb.reciever();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let local = tokio::task::LocalSet::new();

            local.block_on(&rt, async {
                hb.run().await;
            });
        });

        let task_mgr = mlua_scheduler::taskmgr::add_scheduler(&lua, hb_recv.clone(), |_, e| {
            eprintln!("Error: {}", e);
            Ok(())
        })
        .unwrap();
        let task_mgr_ref = task_mgr.clone();
        local.spawn_local(async move {
            task_mgr_ref
                .run()
                .await
                .expect("Failed to run task manager");
        });

        lua.globals()
            .set("_OS", OS.to_lowercase())
            .expect("Failed to set _OS global");

        lua.globals()
            .set(
                "task",
                mlua_scheduler::userdata::table(&lua).expect("Failed to create table"),
            )
            .expect("Failed to set task global");

        smol::block_on(spawn_script(lua.clone(), cli.path)).expect("Failed to spawn script");

        loop {
            //println!("Task manager len: {}", task_mgr.len());
            if task_mgr.is_empty() {
                break;
            }

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        std::process::exit(0);
    });
}
