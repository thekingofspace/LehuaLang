use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use mlua::{Function, Lua, MetaMethod, Table, UserData, UserDataMethods, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, connect_async, MaybeTlsStream, WebSocketStream};

use super::bytestream::value_bytes;
use super::events::{fire_close, spawn_handler, spawn_task, CloseState, Stop};
use crate::engine::VmScheduler;
use crate::error::LehuaError;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = SplitStream<WsStream>;

pub struct WebSocket {
    writer: Rc<Mutex<WsSink>>,
    reader: Rc<Mutex<WsSource>>,
    peer: String,
    sched: Rc<VmScheduler>,
    stop: Arc<Stop>,
    close_state: Rc<CloseState>,
    msg_bound: Cell<bool>,
}

fn socket_from_stream(stream: WsStream, peer: String, sched: Rc<VmScheduler>) -> WebSocket {
    let (w, r) = stream.split();
    WebSocket {
        writer: Rc::new(Mutex::new(w)),
        reader: Rc::new(Mutex::new(r)),
        peer,
        sched,
        stop: Stop::new(),
        close_state: CloseState::new(),
        msg_bound: Cell::new(false),
    }
}

fn message_to_table(lua: &Lua, msg: Message) -> mlua::Result<Value> {
    let t = lua.create_table()?;
    match msg {
        Message::Text(s) => {
            t.set("type", "text")?;
            t.set("data", lua.create_string(s)?)?;
        }
        Message::Binary(b) => {
            t.set("type", "binary")?;
            t.set("data", lua.create_string(b)?)?;
        }
        Message::Ping(b) => {
            t.set("type", "ping")?;
            t.set("data", lua.create_string(b)?)?;
        }
        Message::Pong(b) => {
            t.set("type", "pong")?;
            t.set("data", lua.create_string(b)?)?;
        }
        Message::Close(frame) => {
            t.set("type", "close")?;
            if let Some(f) = frame {
                t.set("code", u16::from(f.code))?;
                t.set("reason", lua.create_string(f.reason.as_bytes())?)?;
            }
        }
        Message::Frame(_) => {
            t.set("type", "frame")?;
        }
    }
    Ok(Value::Table(t))
}

impl UserData for WebSocket {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("on", |lua, this, (event, cb): (String, Function)| {
            match event.as_str() {
                "message" => {
                    if this.msg_bound.get() {
                        return Err(LehuaError::msg(
                            "this websocket already has a message receiver",
                        )
                        .into());
                    }
                    if this.stop.is_stopped() {
                        return Err(LehuaError::msg("this websocket is closed").into());
                    }
                    this.msg_bound.set(true);
                    let lua = lua.clone();
                    let sched = this.sched.clone();
                    let reader = this.reader.clone();
                    let stop = this.stop.clone();
                    let close_state = this.close_state.clone();
                    spawn_task(&this.sched, async move {
                        loop {
                            let next = tokio::select! {
                                _ = stop.wait() => break,
                                r = async { reader.lock().await.next().await } => r,
                            };
                            match next {
                                Some(Ok(Message::Close(frame))) => {
                                    let (code, reason) = match frame {
                                        Some(f) => (
                                            Some(u16::from(f.code)),
                                            Some(f.reason.to_string()),
                                        ),
                                        None => (None, None),
                                    };
                                    fire_close(
                                        &lua,
                                        &sched,
                                        "websocket close",
                                        &close_state,
                                        (code, reason),
                                    );
                                    return;
                                }
                                Some(Ok(msg)) => match message_to_table(&lua, msg) {
                                    Ok(v) => {
                                        spawn_handler(&lua, &sched, "websocket message", &cb, v)
                                    }
                                    Err(e) => {
                                        fire_close(
                                            &lua,
                                            &sched,
                                            "websocket close",
                                            &close_state,
                                            (None::<u16>, Some(e.to_string())),
                                        );
                                        return;
                                    }
                                },
                                Some(Err(e)) => {
                                    fire_close(
                                        &lua,
                                        &sched,
                                        "websocket close",
                                        &close_state,
                                        (None::<u16>, Some(e.to_string())),
                                    );
                                    return;
                                }
                                None => {
                                    fire_close(
                                        &lua,
                                        &sched,
                                        "websocket close",
                                        &close_state,
                                        (),
                                    );
                                    return;
                                }
                            }
                        }
                        fire_close(&lua, &sched, "websocket close", &close_state, ());
                    });
                }
                "close" => {
                    if this.close_state.fired.get() {
                        spawn_handler(lua, &this.sched, "websocket close", &cb, ());
                    } else {
                        *this.close_state.cb.borrow_mut() = Some(cb);
                    }
                }
                other => {
                    return Err(LehuaError::msg(format!(
                        "unknown websocket event '{other}' (expected \"message\" or \"close\")"
                    ))
                    .into())
                }
            }
            Ok(())
        });

        m.add_async_method("send", |_, this, text: mlua::LuaString| {
            let writer = this.writer.clone();
            async move {
                let s = text.to_str()?.to_string();
                writer
                    .lock()
                    .await
                    .send(Message::text(s))
                    .await
                    .map_err(mlua::Error::external)?;
                Ok(())
            }
        });

        m.add_async_method("sendBinary", |_, this, data: Value| {
            let writer = this.writer.clone();
            async move {
                let bytes = value_bytes(&data)?;
                writer
                    .lock()
                    .await
                    .send(Message::binary(bytes))
                    .await
                    .map_err(mlua::Error::external)?;
                Ok(())
            }
        });

        m.add_async_method("ping", |_, this, data: Option<mlua::LuaString>| {
            let writer = this.writer.clone();
            async move {
                let payload = data.map(|d| d.as_bytes().to_vec()).unwrap_or_default();
                writer
                    .lock()
                    .await
                    .send(Message::Ping(payload.into()))
                    .await
                    .map_err(mlua::Error::external)?;
                Ok(())
            }
        });

        m.add_async_method("receive", |lua, this, ()| {
            let reader = this.reader.clone();
            let stop = this.stop.clone();
            async move {
                if stop.is_stopped() {
                    return Ok(Value::Nil);
                }
                let next = tokio::select! {
                    _ = stop.wait() => return Ok(Value::Nil),
                    r = async { reader.lock().await.next().await } => r,
                };
                match next {
                    Some(Ok(msg)) => message_to_table(&lua, msg),
                    Some(Err(e)) => Err(mlua::Error::external(e)),
                    None => Ok(Value::Nil),
                }
            }
        });

        m.add_async_method(
            "close",
            |lua, this, (code, reason): (Option<u16>, Option<String>)| {
                let writer = this.writer.clone();
                let stop = this.stop.clone();
                let sched = this.sched.clone();
                let close_state = this.close_state.clone();
                async move {
                    let frame = code.map(|c| CloseFrame {
                        code: c.into(),
                        reason: reason.clone().unwrap_or_default().into(),
                    });
                    let _ = writer.lock().await.send(Message::Close(frame)).await;
                    stop.stop();
                    fire_close(&lua, &sched, "websocket close", &close_state, (code, reason));
                    Ok(())
                }
            },
        );

        m.add_method("peerAddress", |_, this, ()| Ok(this.peer.clone()));

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("WebSocket({})", this.peer))
        });
    }
}

pub struct WebSocketServer {
    listener: Rc<TcpListener>,
    stop: Arc<Stop>,
    local: String,
    sched: Rc<VmScheduler>,
    conn_bound: Cell<bool>,
}

impl UserData for WebSocketServer {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("on", |lua, this, (event, cb): (String, Function)| {
            if event != "connection" {
                return Err(LehuaError::msg(format!(
                    "unknown websocket server event '{event}' (expected \"connection\")"
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
                        Ok((stream, addr)) => {
                            let peer = addr.to_string();
                            match accept_async(MaybeTlsStream::Plain(stream)).await {
                                Ok(ws) => {
                                    let conn = socket_from_stream(ws, peer, sched.clone());
                                    match lua.create_userdata(conn) {
                                        Ok(ud) => spawn_handler(
                                            &lua,
                                            &sched,
                                            "websocket connection",
                                            &cb,
                                            ud,
                                        ),
                                        Err(e) => eprintln!(
                                            "lehua: net: websocket connection handler error: {e}"
                                        ),
                                    }
                                }
                                Err(e) => {
                                    eprintln!("lehua: net: websocket handshake error: {e}")
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("lehua: net: websocket accept error: {e}");
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
                let (stream, addr) = tokio::select! {
                    _ = stop.wait() => return Ok(Value::Nil),
                    r = listener.accept() => r.map_err(mlua::Error::external)?,
                };
                let peer = addr.to_string();
                let ws = accept_async(MaybeTlsStream::Plain(stream))
                    .await
                    .map_err(mlua::Error::external)?;
                Ok(Value::UserData(
                    lua.create_userdata(socket_from_stream(ws, peer, sched))?,
                ))
            }
        });

        m.add_method("address", |_, this, ()| Ok(this.local.clone()));

        m.add_method("close", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("WebSocketServer({})", this.local))
        });
    }
}

pub fn install(lua: &Lua, net: &Table, sched: Rc<VmScheduler>) -> mlua::Result<()> {
    let ws = lua.create_table()?;

    {
        let sched = sched.clone();
        ws.set(
            "connect",
            lua.create_async_function(move |lua, url: String| {
                let sched = sched.clone();
                async move {
                    if !url.starts_with("ws://") && !url.starts_with("wss://") {
                        return Err(LehuaError::msg(
                            "ws.connect: url must start with ws:// or wss://",
                        )
                        .into());
                    }
                    let (stream, _resp) = connect_async(url.as_str())
                        .await
                        .map_err(mlua::Error::external)?;
                    Ok(Value::UserData(lua.create_userdata(socket_from_stream(
                        stream,
                        url,
                        sched,
                    ))?))
                }
            })?,
        )?;
    }

    {
        let sched = sched.clone();
        ws.set(
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
                    let server = WebSocketServer {
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

    net.set("ws", ws)?;
    Ok(())
}
