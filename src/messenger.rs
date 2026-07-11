use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use async_channel::{Receiver, Sender};
use mlua::{Function, Lua, MetaMethod, MultiValue, UserData, UserDataMethods, Value};

use crate::engine::{SchedGuard, VmScheduler};
use crate::error::LehuaError;
use crate::portable::PortableValue;

struct Event {
    sub_id: u64,
    args: Vec<PortableValue>,
}

struct HubEntry {
    id: u64,
    topic: String,
    tx: Sender<Event>,
}

fn hub() -> &'static Mutex<Vec<HubEntry>> {
    static HUB: OnceLock<Mutex<Vec<HubEntry>>> = OnceLock::new();
    HUB.get_or_init(|| Mutex::new(Vec::new()))
}

static NEXT_SUB_ID: AtomicU64 = AtomicU64::new(1);

struct Handler {
    func: Function,
    once: bool,
    _guard: SchedGuard,
}

struct Local {
    sched: Rc<VmScheduler>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    handlers: RefCell<HashMap<u64, Handler>>,
    pump_started: Cell<bool>,
}

impl Local {
    fn unsubscribe(&self, id: u64) -> bool {
        let removed = self.handlers.borrow_mut().remove(&id).is_some();
        if removed {
            hub().lock().unwrap().retain(|e| e.id != id);
        }
        removed
    }
}

fn spawn_callback(lua: &Lua, sched: &Rc<VmScheduler>, func: &Function, args: MultiValue) {
    let thread = match lua.create_thread(func.clone()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lehua: messenger handler error: {}", crate::error::pretty(&e));
            return;
        }
    };
    let fut = match thread.into_async::<()>(args) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("lehua: messenger handler error: {}", crate::error::pretty(&e));
            return;
        }
    };
    let guard = SchedGuard::new(sched);
    tokio::task::spawn_local(async move {
        let _guard = guard;
        if let Err(e) = crate::engine::catch_panics(fut).await {
            eprintln!("lehua: messenger handler error: {}", crate::error::pretty(&e));
        }
    });
}

fn ensure_pump(lua: &Lua, local: &Rc<Local>) {
    if local.pump_started.get() {
        return;
    }
    local.pump_started.set(true);
    let rx = local.rx.clone();
    let lua = lua.clone();
    let local = local.clone();
    tokio::task::spawn_local(async move {
        while let Ok(ev) = rx.recv().await {
            let found = local
                .handlers
                .borrow()
                .get(&ev.sub_id)
                .map(|h| (h.func.clone(), h.once));
            let Some((func, once)) = found else {
                continue;
            };
            if once {
                local.unsubscribe(ev.sub_id);
            }
            let mut vals: Vec<Value> = Vec::with_capacity(ev.args.len());
            let mut ok = true;
            for a in ev.args {
                match a.into_lua(&lua) {
                    Ok(v) => vals.push(v),
                    Err(e) => {
                        eprintln!(
                            "lehua: messenger delivery error: {}",
                            crate::error::pretty(&e)
                        );
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                spawn_callback(&lua, &local.sched, &func, MultiValue::from_vec(vals));
            }
        }
    });
}

fn subscribe(
    lua: &Lua,
    local: &Rc<Local>,
    topic: String,
    func: Function,
    once: bool,
) -> mlua::Result<Subscription> {
    if topic.is_empty() {
        return Err(LehuaError::msg("messenger: topic cannot be empty").into());
    }
    let id = NEXT_SUB_ID.fetch_add(1, Ordering::Relaxed);
    local.handlers.borrow_mut().insert(
        id,
        Handler {
            func,
            once,
            _guard: SchedGuard::new(&local.sched),
        },
    );
    hub().lock().unwrap().push(HubEntry {
        id,
        topic: topic.clone(),
        tx: local.tx.clone(),
    });
    ensure_pump(lua, local);
    Ok(Subscription {
        id,
        topic,
        local: local.clone(),
    })
}

pub struct Messenger {
    local: Rc<Local>,
}

impl UserData for Messenger {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("Publish", |_, _, (topic, args): (String, MultiValue)| {
            let mut portable = Vec::with_capacity(args.len());
            for v in args.iter() {
                let p = PortableValue::from_lua(v).map_err(|e| -> mlua::Error {
                    match e {
                        LehuaError::NotPortable(t) => LehuaError::msg(format!(
                            "messenger: a value of type '{t}' cannot be published to other threads"
                        ))
                        .into(),
                        other => other.into(),
                    }
                })?;
                portable.push(p);
            }
            let mut entries = hub().lock().unwrap();
            entries.retain(|e| !e.tx.is_closed());
            let mut delivered = 0usize;
            for e in entries.iter() {
                if e.topic == topic {
                    if e
                        .tx
                        .try_send(Event {
                            sub_id: e.id,
                            args: portable.clone(),
                        })
                        .is_ok()
                    {
                        delivered += 1;
                    }
                }
            }
            Ok(delivered)
        });

        m.add_method(
            "Subscribe",
            |lua, this, (topic, func): (String, Function)| {
                subscribe(lua, &this.local, topic, func, false)
            },
        );

        m.add_method("Once", |lua, this, (topic, func): (String, Function)| {
            subscribe(lua, &this.local, topic, func, true)
        });

        m.add_method("Subscribers", |_, _, topic: String| {
            let mut entries = hub().lock().unwrap();
            entries.retain(|e| !e.tx.is_closed());
            Ok(entries.iter().filter(|e| e.topic == topic).count())
        });

        m.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("Messenger"));
    }
}

pub struct Subscription {
    id: u64,
    topic: String,
    local: Rc<Local>,
}

impl UserData for Subscription {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("Unsubscribe", |_, this, ()| Ok(this.local.unsubscribe(this.id)));

        m.add_method("Topic", |_, this, ()| Ok(this.topic.clone()));

        m.add_method("IsActive", |_, this, ()| {
            Ok(this.local.handlers.borrow().contains_key(&this.id))
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("Subscription({})", this.topic))
        });
    }
}

pub fn install(lua: &Lua, sched: Rc<VmScheduler>) -> mlua::Result<()> {
    let (tx, rx) = async_channel::unbounded::<Event>();
    let local = Rc::new(Local {
        sched,
        tx,
        rx,
        handlers: RefCell::new(HashMap::new()),
        pump_started: Cell::new(false),
    });
    lua.globals().set("messenger", Messenger { local })
}
