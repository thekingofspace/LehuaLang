use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use mlua::{Function, Lua, MetaMethod, MultiValue, Table, UserData, UserDataMethods, Value};
use tokio::net::UdpSocket;

use super::bytestream::value_bytes;
use super::events::{spawn_handler, spawn_task, Stop};
use crate::engine::VmScheduler;
use crate::error::LehuaError;

pub struct UdpHandle {
    socket: Rc<UdpSocket>,
    local: String,
    sched: Rc<VmScheduler>,
    stop: Arc<Stop>,
    msg_bound: Cell<bool>,
}

fn check_open(stop: &Stop) -> mlua::Result<()> {
    if stop.is_stopped() {
        return Err(LehuaError::msg("this udp socket is closed").into());
    }
    Ok(())
}

impl UserData for UdpHandle {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("on", |lua, this, (event, cb): (String, Function)| {
            if event != "message" {
                return Err(LehuaError::msg(format!(
                    "unknown udp event '{event}' (expected \"message\")"
                ))
                .into());
            }
            if this.msg_bound.get() {
                return Err(LehuaError::msg("this socket already has a message receiver").into());
            }
            check_open(&this.stop)?;
            this.msg_bound.set(true);
            let lua = lua.clone();
            let sched = this.sched.clone();
            let socket = this.socket.clone();
            let stop = this.stop.clone();
            spawn_task(&this.sched, async move {
                let mut buf = vec![0u8; 65536];
                loop {
                    let received = tokio::select! {
                        _ = stop.wait() => break,
                        r = socket.recv_from(&mut buf) => r,
                    };
                    match received {
                        Ok((n, addr)) => match lua.create_string(&buf[..n]) {
                            Ok(data) => spawn_handler(
                                &lua,
                                &sched,
                                "udp message",
                                &cb,
                                (data, addr.ip().to_string(), addr.port()),
                            ),
                            Err(e) => {
                                eprintln!("lehua: net: udp message handler error: {e}");
                                break;
                            }
                        },
                        Err(e) => {
                            eprintln!("lehua: net: udp receive error: {e}");
                        }
                    }
                }
            });
            Ok(())
        });

        m.add_async_method(
            "sendTo",
            |_, this, (data, host, port): (Value, String, u16)| {
                let socket = this.socket.clone();
                let stop = this.stop.clone();
                async move {
                    check_open(&stop)?;
                    let bytes = value_bytes(&data)?;
                    let n = socket
                        .send_to(&bytes, (host.as_str(), port))
                        .await
                        .map_err(mlua::Error::external)?;
                    Ok(n)
                }
            },
        );

        m.add_async_method("recvFrom", |lua, this, ()| {
            let socket = this.socket.clone();
            let stop = this.stop.clone();
            async move {
                check_open(&stop)?;
                let mut buf = vec![0u8; 65536];
                let (n, addr) = tokio::select! {
                    _ = stop.wait() => return Ok(MultiValue::from_vec(vec![Value::Nil])),
                    r = socket.recv_from(&mut buf) => r.map_err(mlua::Error::external)?,
                };
                buf.truncate(n);
                let out = MultiValue::from_vec(vec![
                    Value::String(lua.create_string(buf)?),
                    Value::String(lua.create_string(addr.ip().to_string())?),
                    Value::Integer(addr.port() as i64),
                ]);
                Ok(out)
            }
        });

        m.add_async_method("connect", |_, this, (host, port): (String, u16)| {
            let socket = this.socket.clone();
            let stop = this.stop.clone();
            async move {
                check_open(&stop)?;
                socket
                    .connect((host.as_str(), port))
                    .await
                    .map_err(mlua::Error::external)?;
                Ok(())
            }
        });

        m.add_async_method("send", |_, this, data: Value| {
            let socket = this.socket.clone();
            let stop = this.stop.clone();
            async move {
                check_open(&stop)?;
                let bytes = value_bytes(&data)?;
                let n = socket.send(&bytes).await.map_err(mlua::Error::external)?;
                Ok(n)
            }
        });

        m.add_async_method("recv", |lua, this, ()| {
            let socket = this.socket.clone();
            let stop = this.stop.clone();
            async move {
                check_open(&stop)?;
                let mut buf = vec![0u8; 65536];
                let n = tokio::select! {
                    _ = stop.wait() => return Ok(Value::Nil),
                    r = socket.recv(&mut buf) => r.map_err(mlua::Error::external)?,
                };
                buf.truncate(n);
                Ok(Value::String(lua.create_string(buf)?))
            }
        });

        m.add_async_method("broadcast", |_, this, on: Option<bool>| {
            let socket = this.socket.clone();
            async move {
                socket
                    .set_broadcast(on.unwrap_or(true))
                    .map_err(mlua::Error::external)?;
                Ok(())
            }
        });

        m.add_method("localAddress", |_, this, ()| Ok(this.local.clone()));

        m.add_method("close", |_, this, ()| {
            this.stop.stop();
            Ok(())
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("UdpSocket({})", this.local))
        });
    }
}

pub fn install(lua: &Lua, net: &Table, sched: Rc<VmScheduler>) -> mlua::Result<()> {
    let udp = lua.create_table()?;

    udp.set(
        "bind",
        lua.create_async_function(
            move |lua, (host, port): (Option<String>, Option<u16>)| {
                let sched = sched.clone();
                async move {
                    let host = host.unwrap_or_else(|| "0.0.0.0".to_string());
                    let port = port.unwrap_or(0);
                    let socket = UdpSocket::bind((host.as_str(), port))
                        .await
                        .map_err(mlua::Error::external)?;
                    let local = socket
                        .local_addr()
                        .map(|a| a.to_string())
                        .unwrap_or_default();
                    let handle = UdpHandle {
                        socket: Rc::new(socket),
                        local,
                        sched,
                        stop: Stop::new(),
                        msg_bound: Cell::new(false),
                    };
                    Ok(Value::UserData(lua.create_userdata(handle)?))
                }
            },
        )?,
    )?;

    net.set("udp", udp)?;
    Ok(())
}
