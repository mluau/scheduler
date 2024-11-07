use clap::Parser;
use mlua_scheduler::{scheduler::Scheduler, traits::LuaSchedulerMethods};
use smol::fs;
use std::path::PathBuf;

#[derive(Debug, Parser)]
struct Cli {
    path: PathBuf,
}

async fn spawn_script(lua: mlua::Lua, path: PathBuf) -> mlua::Result<()> {
    let chunk = lua
        .load(fs::read_to_string(&path).await?)
        .set_name(fs::canonicalize(&path).await?.to_string_lossy());

    lua.spawn_thread(
        lua.create_thread(chunk.into_function()?)?,
        mlua_scheduler::SpawnProt::Spawn,
        (),
    )?;

    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let lua = mlua::Lua::new();
    let scheduler = Scheduler::new().setup(&lua);

    mlua_task_std::inject_globals(&lua).unwrap();

    smol::block_on(spawn_script(lua.clone(), cli.path)).expect("Failed to spawn script");
    smol::block_on(scheduler.run()).expect("Failed to run scheduler");

    if scheduler.errors.is_empty() {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}
