use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender};
use mlua::{Function, Lua, MultiValue, UserData, UserDataMethods, Value, VmState};

use crate::engine::{self, Engine};
use crate::error::LehuaError;
use crate::portable::PortableValue;

struct WorkerSignal {
    stop: AtomicBool,
    wake: tokio::sync::Notify,
}

struct WorkerEntry {
    serial: u64,
    signal: Arc<WorkerSignal>,
    join: std::thread::JoinHandle<()>,
}

fn workers() -> &'static Mutex<Vec<WorkerEntry>> {
    static WORKERS: OnceLock<Mutex<Vec<WorkerEntry>>> = OnceLock::new();
    WORKERS.get_or_init(|| Mutex::new(Vec::new()))
}

static NEXT_WORKER: AtomicU64 = AtomicU64::new(1);

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

pub fn shutdown_all() {
    let entries: Vec<WorkerEntry> = std::mem::take(&mut *workers().lock().unwrap());
    if entries.is_empty() {
        return;
    }
    for e in &entries {
        e.signal.stop.store(true, Ordering::SeqCst);
        e.signal.wake.notify_one();
    }
    let me = std::thread::current().id();
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    for e in entries {
        if e.join.thread().id() == me {
            continue;
        }
        while !e.join.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if e.join.is_finished() {
            let _ = e.join.join();
        }
    }
}

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
    let from_id = from_id.to_string();
    lua.create_async_function(move |lua, args: MultiValue| {
        let engine = engine.clone();
        let from_id = from_id.clone();
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

            let chain = engine.from_chain(&lua, &from_id);
            let worker_id =
                engine
                    .resolver
                    .resolve_worker(&chain, &from_id, &path, engine.provider.as_ref())?;

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
    let serial = NEXT_WORKER.fetch_add(1, Ordering::Relaxed);
    let signal = Arc::new(WorkerSignal {
        stop: AtomicBool::new(false),
        wake: tokio::sync::Notify::new(),
    });
    let thread_signal = signal.clone();
    let builder = std::thread::Builder::new().name(format!("lehua-worker:{worker_id}"));
    let spawned = builder.spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("lehua: worker '{worker_id}' failed to start runtime: {e}");
                workers().lock().unwrap().retain(|e| e.serial != serial);
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
            let interrupt_signal = thread_signal.clone();
            lua.set_interrupt(move |_| {
                if interrupt_signal.stop.load(Ordering::Relaxed) {
                    Err(LehuaError::msg("worker stopped: the program is closing").into())
                } else {
                    Ok(VmState::Continue)
                }
            });
            let chan = match lua.create_userdata(worker_port) {
                Ok(ud) => Value::UserData(ud),
                Err(e) => {
                    eprintln!("lehua: worker '{worker_id}' failed to bind channel: {e}");
                    return;
                }
            };
            tokio::select! {
                res = engine::run_entry(lua.clone(), ctx.clone(), &worker_id, Some(chan), args) => {
                    if let Err(e) = res {
                        if !thread_signal.stop.load(Ordering::Relaxed) {
                            eprintln!("lehua: worker '{worker_id}' error: {}", crate::error::pretty(&e));
                        }
                    }
                }
                _ = thread_signal.wake.notified() => {
                    lua.remove_interrupt();
                    ctx.sched.run_close_async().await;
                    crate::messenger::shutdown(&lua);
                }
            }
        }));
        drop(local);
        rt.shutdown_background();
        workers().lock().unwrap().retain(|e| e.serial != serial);
    });

    match spawned {
        Ok(join) => workers().lock().unwrap().push(WorkerEntry { serial, signal, join }),
        Err(e) => eprintln!("lehua: failed to spawn worker thread for '{label}': {e}"),
    }
}
