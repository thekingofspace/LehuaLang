use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use mlua::{Function, Lua, MetaMethod, Table, UserData, UserDataMethods, Value};
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use super::bytestream;
use super::events::{
    fire_close, spawn_handler, spawn_read_pump, spawn_task, CloseState, ReadEvent, Stop,
};
use crate::engine::VmScheduler;
use crate::error::LehuaError;

pub struct TcpConn {
    reader: Rc<Mutex<BufReader<OwnedReadHalf>>>,
    writer: Rc<Mutex<OwnedWriteHalf>>,
    peer: String,
    local: String,
    sched: Rc<VmScheduler>,
    stop: Arc<Stop>,
    close_state: Rc<CloseState>,
    pump_bound: Cell<bool>,
}

pub fn conn_from_stream(stream: TcpStream, sched: Rc<VmScheduler>) -> TcpConn {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let local = stream
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let _ = stream.set_nodelay(true);
    let (r, w) = stream.into_split();
    TcpConn {
        reader: Rc::new(Mutex::new(BufReader::new(r))),
        writer: Rc::new(Mutex::new(w)),
        peer,
        local,
        sched,
        stop: Stop::new(),
        close_state: CloseState::new(),
        pump_bound: Cell::new(false),
    }
}

impl UserData for TcpConn {
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
                        "tcp data",
                        "tcp close",
                        this.reader.clone(),
                        this.stop.clone(),
                        this.close_state.clone(),
                        mode,
                        cb,
                    );
                }
                "close" => {
                    if this.close_state.fired.get() {
                        spawn_handler(lua, &this.sched, "tcp close", &cb, ());
                    } else {
                        *this.close_state.cb.borrow_mut() = Some(cb);
                    }
                }
                other => {
                    return Err(LehuaError::msg(format!(
                        "unknown tcp connection event '{other}' (expected \"data\", \"line\", or \"close\")"
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

        m.add_async_method("readUntil", |lua, this, delim: mlua::LuaString| {
            let reader = this.reader.clone();
            async move {
                let bytes = delim.as_bytes();
                let d = *bytes
                    .first()
                    .ok_or_else(|| LehuaError::msg("readUntil needs a one byte delimiter"))?;
                let chunk = bytestream::read_until(&mut *reader.lock().await, d).await?;
                bytestream::opt_bytes_to_lua(&lua, chunk)
            }
        });

        m.add_async_method("readAll", |lua, this, ()| {
            let reader = this.reader.clone();
            async move {
                let bytes = bytestream::read_all(&mut *reader.lock().await).await?;
                Ok(Value::String(lua.create_string(bytes)?))
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

        m.add_method("peerAddress", |_, this, ()| Ok(this.peer.clone()));
        m.add_method("localAddress", |_, this, ()| Ok(this.local.clone()));

        m.add_async_method("close", |lua, this, ()| {
            let writer = this.writer.clone();
            let stop = this.stop.clone();
            let sched = this.sched.clone();
            let close_state = this.close_state.clone();
            async move {
                stop.stop();
                let _ = writer.lock().await.shutdown().await;
                fire_close(&lua, &sched, "tcp close", &close_state, ());
                Ok(())
            }
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("TcpConnection({})", this.peer))
        });
    }
}

pub struct TcpServer {
    listener: Rc<TcpListener>,
    stop: Arc<Stop>,
    local: String,
    sched: Rc<VmScheduler>,
    conn_bound: Cell<bool>,
}

impl UserData for TcpServer {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("on", |lua, this, (event, cb): (String, Function)| {
            if event != "connection" {
                return Err(LehuaError::msg(format!(
                    "unknown tcp server event '{event}' (expected \"connection\")"
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
                        Ok((stream, _)) => {
                            let conn = conn_from_stream(stream, sched.clone());
                            match lua.create_userdata(conn) {
                                Ok(ud) => {
                                    spawn_handler(&lua, &sched, "tcp connection", &cb, ud)
                                }
                                Err(e) => {
                                    eprintln!("lehua: net: tcp connection handler error: {e}")
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("lehua: net: tcp accept error: {e}");
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
                        let (stream, _) = r.map_err(mlua::Error::external)?;
                        Ok(Value::UserData(
                            lua.create_userdata(conn_from_stream(stream, sched))?,
                        ))
                    }
                }
            }
        });

        m.add_method("address", |_, this, ()| Ok(this.local.clone()));

        m.add_method("close", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("TcpServer({})", this.local))
        });
    }
}

pub fn install(lua: &Lua, net: &Table, sched: Rc<VmScheduler>) -> mlua::Result<()> {
    let tcp = lua.create_table()?;

    {
        let sched = sched.clone();
        tcp.set(
            "connect",
            lua.create_async_function(move |lua, (host, port): (String, u16)| {
                let sched = sched.clone();
                async move {
                    let stream = TcpStream::connect((host.as_str(), port))
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
        tcp.set(
            "listen",
            lua.create_async_function(move |lua, (host, port): (Option<String>, u16)| {
                let sched = sched.clone();
                async move {
                    let host = host.unwrap_or_else(|| "0.0.0.0".to_string());
                    let listener = TcpListener::bind((host.as_str(), port))
                        .await
                        .map_err(mlua::Error::external)?;
                    let local = listener
                        .local_addr()
                        .map(|a| a.to_string())
                        .unwrap_or_default();
                    let server = TcpServer {
                        listener: Rc::new(listener),
                        stop: Stop::new(),
                        local,
                        sched,
                        conn_bound: Cell::new(false),
                    };
                    Ok(Value::UserData(lua.create_userdata(server)?))
                }
            })?,
        )?;
    }

    net.set("tcp", tcp)?;
    Ok(())
}
