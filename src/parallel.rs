use std::sync::Arc;

use async_channel::{Receiver, Sender};
use mlua::{Function, Lua, MultiValue, UserData, UserDataMethods, Value};

use crate::engine::{self, Engine};
use crate::error::LehuaError;
use crate::portable::PortableValue;
use crate::vpath;

pub struct Port {
    tx: Sender<PortableValue>,
    rx: Receiver<PortableValue>,
}

impl UserData for Port {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Push", |_, this, value: Value| {
            if value.is_nil() {
                return Err(mlua::Error::external(LehuaError::msg(
                    "Port:Push(nil) is not allowed - nil is the closed-channel sentinel returned by Pop",
                )));
            }
            let pv = PortableValue::from_lua(&value)?;
            Ok(this.tx.try_send(pv).is_ok())
        });

        methods.add_async_method("Pop", |lua, this, ()| {
            let rx = this.rx.clone();
            async move {
                match rx.recv().await {
                    Ok(v) => v.into_lua(&lua),
                    Err(_) => Ok(Value::Nil),
                }
            }
        });

        methods.add_method("TryPop", |lua, this, ()| match this.rx.try_recv() {
            Ok(v) => v.into_lua(lua),
            Err(_) => Ok(Value::Nil),
        });

        methods.add_method("Close", |_, this, ()| {
            this.tx.close();
            this.rx.close();
            Ok(())
        });

        methods.add_method("IsClosed", |_, this, ()| Ok(this.tx.is_closed()));
    }
}

pub fn make_parallel(lua: &Lua, engine: Arc<Engine>, from_id: &str) -> mlua::Result<Function> {
    let from_dir = vpath::dirname(from_id);
    lua.create_async_function(move |lua, args: MultiValue| {
        let engine = engine.clone();
        let from_dir = from_dir.clone();
        async move {
            let mut it = args.into_iter();
            let path = match it.next() {
                Some(Value::String(s)) => s.to_string_lossy().to_string(),
                _ => {
                    return Err(mlua::Error::external(LehuaError::msg(
                        "parallel(path, ...) expects a file path as its first argument",
                    )))
                }
            };
            let extra: Vec<PortableValue> = it
                .map(|v| PortableValue::from_lua(&v))
                .collect::<crate::error::Result<_>>()?;

            let worker_id =
                engine
                    .resolver
                    .resolve_worker(&from_dir, &path, engine.provider.as_ref())?;

            let (a_tx, a_rx) = async_channel::unbounded::<PortableValue>();
            let (b_tx, b_rx) = async_channel::unbounded::<PortableValue>();
            let parent = Port { tx: a_tx, rx: b_rx };
            let worker = Port { tx: b_tx, rx: a_rx };

            spawn_worker(engine.clone(), worker_id, worker, extra);

            Ok(Value::UserData(lua.create_userdata(parent)?))
        }
    })
}

fn spawn_worker(engine: Arc<Engine>, worker_id: String, worker_port: Port, args: Vec<PortableValue>) {
    let label = worker_id.clone();
    let builder = std::thread::Builder::new().name(format!("lehua-worker:{worker_id}"));
    let spawned = builder.spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("lehua: worker '{worker_id}' failed to start runtime: {e}");
                return;
            }
        };

        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async move {
            let (lua, ctx) = match engine::make_vm(engine.clone()) {
                Ok(pair) => pair,
                Err(e) => {
                    eprintln!("lehua: worker '{worker_id}' failed to init VM: {e}");
                    return;
                }
            };
            let chan = match lua.create_userdata(worker_port) {
                Ok(ud) => Value::UserData(ud),
                Err(e) => {
                    eprintln!("lehua: worker '{worker_id}' failed to bind channel: {e}");
                    return;
                }
            };
            if let Err(e) = engine::run_entry(lua, ctx, &worker_id, Some(chan), args).await {
                eprintln!("lehua: worker '{worker_id}' error: {e}");
            }
        }));
    });

    if let Err(e) = spawned {
        eprintln!("lehua: failed to spawn worker thread for '{label}': {e}");
    }
}
