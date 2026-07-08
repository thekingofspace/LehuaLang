use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use interprocess::local_socket::tokio::{Listener, RecvHalf, SendHalf, Stream};
use interprocess::local_socket::traits::tokio::{Listener as _, Stream as _};
use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions, Name, ToFsName, ToNsName,
};
use mlua::{Function, Lua, MetaMethod, Table, UserData, UserDataMethods, Value};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use super::bytestream;
use super::events::{
    fire_close, spawn_handler, spawn_read_pump, spawn_task, CloseState, ReadEvent, Stop,
};
use crate::engine::VmScheduler;
use crate::error::LehuaError;

fn make_name(name: &str) -> mlua::Result<Name<'static>> {
    let looks_like_path = name.contains('/') || name.contains('\\');
    let owned = name.to_string();
    let result = if looks_like_path {
        owned.to_fs_name::<GenericFilePath>()
    } else {
        owned.to_ns_name::<GenericNamespaced>()
    };
    result.map_err(|e| LehuaError::msg(format!("invalid IPC name '{name}': {e}")).into())
}

pub struct IpcConn {
    reader: Rc<Mutex<BufReader<RecvHalf>>>,
    writer: Rc<Mutex<SendHalf>>,
    sched: Rc<VmScheduler>,
    stop: Arc<Stop>,
    close_state: Rc<CloseState>,
    pump_bound: Cell<bool>,
}

fn conn_from_stream(stream: Stream, sched: Rc<VmScheduler>) -> IpcConn {
    let (r, w) = stream.split();
    IpcConn {
        reader: Rc::new(Mutex::new(BufReader::new(r))),
        writer: Rc::new(Mutex::new(w)),
        sched,
        stop: Stop::new(),
        close_state: CloseState::new(),
        pump_bound: Cell::new(false),
    }
}

impl UserData for IpcConn {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("on", |lua, this, (event, cb): (String, Function)| {
            match event.as_str() {
                "data" | "line" => {
                    if this.pump_bound.get() {
                        return Err(LehuaError::msg(
                            "this connection already has a data or line receiver",
                        )
                        .into());
                    }
                    if this.stop.is_stopped() {
                        return Err(LehuaError::msg("this connection is closed").into());
                    }
                    this.pump_bound.set(true);
                    let mode = if event == "data" {
                        ReadEvent::Chunk
                    } else {
                        ReadEvent::Line
                    };
                    spawn_read_pump(
                        lua,
                        &this.sched,
                        "ipc data",
                        "ipc close",
                        this.reader.clone(),
                        this.stop.clone(),
                        this.close_state.clone(),
                        mode,
                        cb,
                    );
                }
                "close" => {
                    if this.close_state.fired.get() {
                        spawn_handler(lua, &this.sched, "ipc close", &cb, ());
                    } else {
                        *this.close_state.cb.borrow_mut() = Some(cb);
                    }
                }
                other => {
                    return Err(LehuaError::msg(format!(
                        "unknown ipc connection event '{other}' (expected \"data\", \"line\", or \"close\")"
                    ))
                    .into())
                }
            }
            Ok(())
        });

        m.add_async_method("read", |lua, this, n: Option<usize>| {
            let reader = this.reader.clone();
            async move {
                let mut guard = reader.lock().await;
                let bytes = match n {
                    Some(n) => bytestream::read_exact(&mut guard, n).await?,
                    None => bytestream::read_some(&mut guard, 64 * 1024).await?,
                };
                if bytes.is_empty() {
                    return Ok(Value::Nil);
                }
                Ok(Value::String(lua.create_string(bytes)?))
            }
        });

        m.add_async_method("readLine", |lua, this, ()| {
            let reader = this.reader.clone();
            async move {
                let line = bytestream::read_line(&mut *reader.lock().await).await?;
                bytestream::opt_bytes_to_lua(&lua, line)
            }
        });

        m.add_async_method("write", |_, this, data: Value| {
            let writer = this.writer.clone();
            async move {
                let bytes = bytestream::value_bytes(&data)?;
                bytestream::write_all(&mut *writer.lock().await, &bytes).await?;
                Ok(())
            }
        });

        m.add_async_method("close", |lua, this, ()| {
            let writer = this.writer.clone();
            let stop = this.stop.clone();
            let sched = this.sched.clone();
            let close_state = this.close_state.clone();
            async move {
                stop.stop();
                let _ = writer.lock().await.shutdown().await;
                fire_close(&lua, &sched, "ipc close", &close_state, ());
                Ok(())
            }
        });

        m.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("IpcConnection"));
    }
}

pub struct IpcServer {
    listener: Rc<Listener>,
    stop: Arc<Stop>,
    name: String,
    sched: Rc<VmScheduler>,
    conn_bound: Cell<bool>,
}

impl UserData for IpcServer {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("on", |lua, this, (event, cb): (String, Function)| {
            if event != "connection" {
                return Err(LehuaError::msg(format!(
                    "unknown ipc server event '{event}' (expected \"connection\")"
                ))
                .into());
            }
            if this.conn_bound.get() {
                return Err(
                    LehuaError::msg("this server already has a connection receiver").into(),
                );
            }
            if this.stop.is_stopped() {
                return Err(LehuaError::msg("this server is closed").into());
            }
            this.conn_bound.set(true);
            let lua = lua.clone();
            let sched = this.sched.clone();
            let listener = this.listener.clone();
            let stop = this.stop.clone();
            spawn_task(&this.sched, async move {
                loop {
                    let accepted = tokio::select! {
                        _ = stop.wait() => break,
                        r = listener.accept() => r,
                    };
                    match accepted {
                        Ok(stream) => {
                            let conn = conn_from_stream(stream, sched.clone());
                            match lua.create_userdata(conn) {
                                Ok(ud) => {
                                    spawn_handler(&lua, &sched, "ipc connection", &cb, ud)
                                }
                                Err(e) => {
                                    eprintln!("lehua: net: ipc connection handler error: {e}")
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("lehua: net: ipc accept error: {e}");
                            break;
                        }
                    }
                }
            });
            Ok(())
        });

        m.add_async_method("accept", |lua, this, ()| {
            let listener = this.listener.clone();
            let stop = this.stop.clone();
            let sched = this.sched.clone();
            async move {
                if stop.is_stopped() {
                    return Ok(Value::Nil);
                }
                tokio::select! {
                    _ = stop.wait() => Ok(Value::Nil),
                    r = listener.accept() => {
                        let stream = r.map_err(mlua::Error::external)?;
                        Ok(Value::UserData(
                            lua.create_userdata(conn_from_stream(stream, sched))?,
                        ))
                    }
                }
            }
        });

        m.add_method("name", |_, this, ()| Ok(this.name.clone()));

        m.add_method("close", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("IpcServer({})", this.name))
        });
    }
}

pub fn install(lua: &Lua, net: &Table, sched: Rc<VmScheduler>) -> mlua::Result<()> {
    let ipc = lua.create_table()?;

    {
        let sched = sched.clone();
        ipc.set(
            "connect",
            lua.create_async_function(move |lua, name: String| {
                let sched = sched.clone();
                async move {
                    let target = make_name(&name)?;
                    let stream = Stream::connect(target)
                        .await
                        .map_err(mlua::Error::external)?;
                    Ok(Value::UserData(
                        lua.create_userdata(conn_from_stream(stream, sched))?,
                    ))
                }
            })?,
        )?;
    }

    {
        let sched = sched.clone();
        ipc.set(
            "listen",
            lua.create_function(move |lua, name: String| {
                let target = make_name(&name)?;
                let listener = ListenerOptions::new()
                    .name(target)
                    .create_tokio()
                    .map_err(mlua::Error::external)?;
                let server = IpcServer {
                    listener: Rc::new(listener),
                    stop: Stop::new(),
                    name,
                    sched: sched.clone(),
                    conn_bound: Cell::new(false),
                };
                Ok(Value::UserData(lua.create_userdata(server)?))
            })?,
        )?;
    }

    net.set("ipc", ipc)?;
    Ok(())
}
