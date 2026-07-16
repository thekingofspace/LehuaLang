mod bytestream;
mod dns;
mod events;
mod ftp;
mod http;
mod ipc;
mod sink;
mod tcp;
mod udp;
mod ws;

use mlua::{Table, Value};

use super::{LibCtx, PathScope};

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let net = lua.create_table()?;
    let scope = PathScope::new(ctx);
    let sched = ctx.sched.clone();

    tcp::install(lua, &net, sched.clone())?;
    udp::install(lua, &net, sched.clone())?;
    ipc::install(lua, &net, sched.clone())?;
    dns::install(lua, &net)?;
    http::install(lua, &net, sched.clone())?;
    ws::install(lua, &net, sched.clone())?;
    ftp::install(lua, &net, scope.clone())?;

    {
        let scope = scope.clone();
        net.set(
            "sink",
            lua.create_async_function(move |lua, opts: Option<Table>| {
                let scope = scope.clone();
                async move {
                    let sink = match opts {
                        Some(o) => match o.get::<Option<String>>("file")? {
                            Some(path) => sink::new_file(scope.clone(), &path).await?,
                            None => sink::new_memory(scope.clone()),
                        },
                        None => sink::new_memory(scope.clone()),
                    };
                    lua.create_userdata(sink)
                }
            })?,
        )?;
    }

    Ok(Value::Table(net))
}
