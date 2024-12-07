const RUNNER: &str = include_str!("../src/lib.rs");

pub async fn spawn_thread_and_get_ret(
    lua: mlua::Lua,
    th: mlua::Thread,
    args: mlua::MultiValue,
) -> mlua::Result<mlua::Value> {
    let mut args = args;
    let (tx, rx) = flume::unbounded();

    args.push_back(mlua::Value::UserData(
        lua.create_userdata(Responder { chan: tx })?,
    ));

    // Wrap function in function that sends return value to channel
    let new_fn: mlua::Function = lua.load(RUNNER).call_async(th).await?;
    let new_th = lua.create_thread(new_fn)?;

    mlua_scheduler::spawn_thread(lua, new_th, args);

    let ret = rx
        .recv_async()
        .await
        .map_err(|_| mlua::Error::external("Channel closed"))?;

    Ok(ret)
}

struct Responder {
    chan: flume::Sender<mlua::Value>,
}

impl mlua::UserData for Responder {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("send", |_, this, value: mlua::Value| {
            this.chan
                .send(value)
                .map_err(|_| mlua::Error::external("Channel closed"))?;

            Ok(())
        });
    }
}
