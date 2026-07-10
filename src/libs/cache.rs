use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant as StdInstant};

use mlua::{
    AnyUserData, Function, Lua, MetaMethod, MultiValue, Table, UserData, UserDataMethods,
    UserDataRef, Value, Vector,
};

use super::LibCtx;
use crate::error::LehuaError;

const MAX_DEPTH: usize = 96;
const MAX_TTL_SECONDS: f64 = 60.0 * 60.0 * 24.0 * 365.0 * 100.0;

#[derive(Clone)]
pub enum RichValue {
    Nil,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(Vec<u8>),
    Buffer(Vec<u8>),
    Vec3(f32, f32, f32),
    Table {
        entries: Vec<(RichValue, RichValue)>,
        meta: Option<Box<RichValue>>,
    },
    Handle(String),
    #[cfg(feature = "lib-datetime")]
    DateTime(i64),
    #[cfg(feature = "lib-regex")]
    Regex { pattern: String, flags: String },
    #[cfg(feature = "lib-sqlite")]
    Sqlite(String),
    #[cfg(feature = "lib-mongo")]
    Mongo(super::mongo::MongoRef),
    #[cfg(feature = "lib-canvas")]
    Canvas {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
}

fn path_note(path: &[String]) -> String {
    if path.is_empty() {
        String::new()
    } else {
        format!(" (at {})", path.join("."))
    }
}

fn key_label(k: &Value) -> String {
    match k {
        Value::String(s) => s.to_string_lossy().to_string(),
        Value::Integer(i) => format!("[{i}]"),
        Value::Number(n) => format!("[{n}]"),
        other => format!("[{}]", other.type_name()),
    }
}

impl RichValue {
    pub fn from_lua(v: &Value) -> mlua::Result<Self> {
        let mut seen = Vec::new();
        let mut path = Vec::new();
        Self::from_lua_inner(v, 0, &mut seen, &mut path)
    }

    fn from_lua_inner(
        v: &Value,
        depth: usize,
        seen: &mut Vec<usize>,
        path: &mut Vec<String>,
    ) -> mlua::Result<Self> {
        if depth > MAX_DEPTH {
            return Err(LehuaError::msg(format!(
                "cache: value is nested too deeply to store{}",
                path_note(path)
            ))
            .into());
        }
        Ok(match v {
            Value::Nil => RichValue::Nil,
            Value::Boolean(b) => RichValue::Bool(*b),
            Value::Integer(i) => RichValue::Int(*i),
            Value::Number(n) => RichValue::Num(*n),
            Value::String(s) => RichValue::Str(s.as_bytes().to_vec()),
            Value::Buffer(b) => RichValue::Buffer(b.to_vec()),
            Value::Vector(vec) => RichValue::Vec3(vec.x(), vec.y(), vec.z()),
            Value::Table(t) => {
                let id = t.to_pointer() as usize;
                if seen.contains(&id) {
                    return Err(LehuaError::msg(format!(
                        "cache: cannot store a table that references itself{}",
                        path_note(path)
                    ))
                    .into());
                }
                seen.push(id);
                let mut entries = Vec::new();
                for pair in t.pairs::<Value, Value>() {
                    let (k, val) = pair?;
                    path.push(key_label(&k));
                    let pk = Self::from_lua_inner(&k, depth + 1, seen, path)?;
                    let pv = Self::from_lua_inner(&val, depth + 1, seen, path)?;
                    path.pop();
                    entries.push((pk, pv));
                }
                let meta = match t.metatable() {
                    Some(mt) => {
                        path.push(String::from("<metatable>"));
                        let m = Self::from_lua_inner(&Value::Table(mt), depth + 1, seen, path)?;
                        path.pop();
                        Some(Box::new(m))
                    }
                    None => None,
                };
                seen.pop();
                RichValue::Table { entries, meta }
            }
            Value::UserData(ud) => Self::from_userdata(ud, path)?,
            Value::Function(_) => {
                return Err(LehuaError::msg(format!(
                    "cache: functions cannot be stored in a mem cache{}",
                    path_note(path)
                ))
                .into())
            }
            Value::Thread(_) => {
                return Err(LehuaError::msg(format!(
                    "cache: coroutines cannot be stored in a mem cache{}",
                    path_note(path)
                ))
                .into())
            }
            Value::LightUserData(_) if *v == Value::NULL => RichValue::Nil,
            other => {
                return Err(LehuaError::msg(format!(
                    "cache: a value of type '{}' cannot be stored in a mem cache{}",
                    other.type_name(),
                    path_note(path)
                ))
                .into())
            }
        })
    }

    fn from_userdata(ud: &AnyUserData, path: &[String]) -> mlua::Result<Self> {
        if let Ok(mc) = ud.borrow::<MemCache>() {
            return Ok(RichValue::Handle(mc.name.clone()));
        }
        #[cfg(feature = "lib-datetime")]
        if let Ok(dt) = ud.borrow::<super::datetime::Instant>() {
            return Ok(RichValue::DateTime(dt.micros));
        }
        #[cfg(feature = "lib-regex")]
        if let Ok(re) = ud.borrow::<super::regex::LuaRegex>() {
            let (pattern, flags) = re.parts();
            return Ok(RichValue::Regex { pattern, flags });
        }
        #[cfg(feature = "lib-sqlite")]
        if let Ok(db) = ud.borrow::<super::sqlite::LuaSqlite>() {
            return match db.share_path() {
                Some(p) => Ok(RichValue::Sqlite(p)),
                None => Err(LehuaError::msg(format!(
                    "cache: this sqlite database cannot be shared across threads because it is closed or in-memory{}",
                    path_note(path)
                ))
                .into()),
            };
        }
        #[cfg(feature = "lib-mongo")]
        if let Some(r) = super::mongo::to_ref(ud) {
            return Ok(RichValue::Mongo(r));
        }
        #[cfg(feature = "lib-canvas")]
        if let Ok(c) = ud.borrow::<super::canvas::Canvas>() {
            let img = c.img.borrow();
            return Ok(RichValue::Canvas {
                width: img.width(),
                height: img.height(),
                pixels: img.as_raw().clone(),
            });
        }
        Err(LehuaError::msg(format!(
            "cache: this userdata cannot be stored in a mem cache{}; file handles, network objects, and anything holding callbacks are thread-local",
            path_note(path)
        ))
        .into())
    }

    fn into_lua<'a>(
        &'a self,
        lua: &'a Lua,
    ) -> Pin<Box<dyn Future<Output = mlua::Result<Value>> + 'a>> {
        Box::pin(async move {
            Ok(match self {
                RichValue::Nil => Value::Nil,
                RichValue::Bool(b) => Value::Boolean(*b),
                RichValue::Int(i) => int_value(*i),
                RichValue::Num(n) => Value::Number(*n),
                RichValue::Str(bytes) => Value::String(lua.create_string(bytes)?),
                RichValue::Buffer(bytes) => Value::Buffer(lua.create_buffer(bytes)?),
                RichValue::Vec3(x, y, z) => Value::Vector(Vector::new(*x, *y, *z)),
                RichValue::Table { entries, meta } => {
                    let t = lua.create_table_with_capacity(0, entries.len())?;
                    for (k, v) in entries {
                        let k = k.into_lua(lua).await?;
                        let v = v.into_lua(lua).await?;
                        t.raw_set(k, v)?;
                    }
                    if let Some(m) = meta {
                        if let Value::Table(mt) = m.into_lua(lua).await? {
                            t.set_metatable(Some(mt))?;
                        }
                    }
                    Value::Table(t)
                }
                RichValue::Handle(name) => memcache_value(lua, name)?,
                #[cfg(feature = "lib-datetime")]
                RichValue::DateTime(micros) => Value::UserData(
                    lua.create_userdata(super::datetime::Instant { micros: *micros })?,
                ),
                #[cfg(feature = "lib-regex")]
                RichValue::Regex { pattern, flags } => Value::UserData(
                    lua.create_userdata(super::regex::LuaRegex::from_parts(pattern, flags)?)?,
                ),
                #[cfg(feature = "lib-sqlite")]
                RichValue::Sqlite(path) => Value::UserData(
                    lua.create_userdata(super::sqlite::LuaSqlite::reopen(path)?)?,
                ),
                #[cfg(feature = "lib-mongo")]
                RichValue::Mongo(r) => super::mongo::from_ref(lua, r.clone()).await?,
                #[cfg(feature = "lib-canvas")]
                RichValue::Canvas {
                    width,
                    height,
                    pixels,
                } => Value::UserData(lua.create_userdata(super::canvas::from_raw_pixels(
                    *width,
                    *height,
                    pixels.clone(),
                )?)?),
            })
        })
    }
}

fn int_value(i: i64) -> Value {
    match i32::try_from(i) {
        Ok(v) => Value::Integer(v.into()),
        Err(_) => Value::Number(i as f64),
    }
}

struct Entry {
    value: RichValue,
    expires_at: Option<StdInstant>,
}

impl Entry {
    fn expired(&self, now: StdInstant) -> bool {
        self.expires_at.map(|at| at <= now).unwrap_or(false)
    }
}

pub struct Store {
    entries: Mutex<HashMap<Vec<u8>, Entry>>,
    default_ttl: Mutex<Option<f64>>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl Store {
    fn new() -> Self {
        Store {
            entries: Mutex::new(HashMap::new()),
            default_ttl: Mutex::new(None),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    fn expiry(&self, ttl: Option<f64>) -> Option<StdInstant> {
        let secs = match ttl {
            Some(t) => Some(t),
            None => *self.default_ttl.lock().unwrap(),
        };
        match secs {
            Some(t) if t.is_finite() && t > 0.0 => {
                Some(StdInstant::now() + Duration::from_secs_f64(t.min(MAX_TTL_SECONDS)))
            }
            _ => None,
        }
    }

    fn prune(&self) {
        let now = StdInstant::now();
        self.entries.lock().unwrap().retain(|_, e| !e.expired(now));
    }
}

fn stores() -> &'static Mutex<HashMap<String, Arc<Store>>> {
    static STORES: OnceLock<Mutex<HashMap<String, Arc<Store>>>> = OnceLock::new();
    STORES.get_or_init(|| Mutex::new(HashMap::new()))
}

static ANON_COUNTER: AtomicU64 = AtomicU64::new(1);

fn ensure_sweeper() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new()
            .name(String::from("lehua-cache-sweeper"))
            .spawn(|| loop {
                std::thread::sleep(Duration::from_secs(1));
                let all: Vec<Arc<Store>> = stores().lock().unwrap().values().cloned().collect();
                for store in all {
                    store.prune();
                }
            });
    });
}

fn open_store(name: &str) -> Arc<Store> {
    ensure_sweeper();
    stores()
        .lock()
        .unwrap()
        .entry(name.to_string())
        .or_insert_with(|| Arc::new(Store::new()))
        .clone()
}

pub fn memcache_name(ud: &AnyUserData) -> Option<String> {
    ud.borrow::<MemCache>().ok().map(|c| c.name.clone())
}

pub fn memcache_value(lua: &Lua, name: &str) -> mlua::Result<Value> {
    let store = open_store(name);
    Ok(Value::UserData(lua.create_userdata(MemCache {
        name: name.to_string(),
        store,
    })?))
}

pub struct MemCache {
    name: String,
    store: Arc<Store>,
}

fn ttl_arg(name: &str, ttl: Option<f64>) -> mlua::Result<Option<f64>> {
    match ttl {
        Some(t) if t.is_nan() => {
            Err(LehuaError::msg(format!("cache: {name}: ttl cannot be NaN")).into())
        }
        other => Ok(other),
    }
}

impl UserData for MemCache {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method(
            "set",
            |_, this, (key, value, ttl): (mlua::LuaString, Value, Option<f64>)| {
                let ttl = ttl_arg("set", ttl)?;
                let rich = RichValue::from_lua(&value)?;
                let expires_at = this.store.expiry(ttl);
                this.store.entries.lock().unwrap().insert(
                    key.as_bytes().to_vec(),
                    Entry {
                        value: rich,
                        expires_at,
                    },
                );
                Ok(())
            },
        );

        m.add_async_method(
            "get",
            |lua, this: UserDataRef<Self>, key: mlua::LuaString| async move {
                let found = {
                    let now = StdInstant::now();
                    let mut map = this.store.entries.lock().unwrap();
                    match map.get(key.as_bytes().as_ref()) {
                        Some(e) if !e.expired(now) => Some(e.value.clone()),
                        Some(_) => {
                            map.remove(key.as_bytes().as_ref());
                            None
                        }
                        None => None,
                    }
                };
                match found {
                    Some(rich) => {
                        this.store.hits.fetch_add(1, Ordering::Relaxed);
                        rich.into_lua(&lua).await
                    }
                    None => {
                        this.store.misses.fetch_add(1, Ordering::Relaxed);
                        Ok(Value::Nil)
                    }
                }
            },
        );

        m.add_async_method(
            "take",
            |lua, this: UserDataRef<Self>, key: mlua::LuaString| async move {
                let found = {
                    let now = StdInstant::now();
                    let mut map = this.store.entries.lock().unwrap();
                    match map.remove(key.as_bytes().as_ref()) {
                        Some(e) if !e.expired(now) => Some(e.value),
                        _ => None,
                    }
                };
                match found {
                    Some(rich) => {
                        this.store.hits.fetch_add(1, Ordering::Relaxed);
                        rich.into_lua(&lua).await
                    }
                    None => {
                        this.store.misses.fetch_add(1, Ordering::Relaxed);
                        Ok(Value::Nil)
                    }
                }
            },
        );

        m.add_method(
            "add",
            |_, this, (key, value, ttl): (mlua::LuaString, Value, Option<f64>)| {
                let ttl = ttl_arg("add", ttl)?;
                let rich = RichValue::from_lua(&value)?;
                let expires_at = this.store.expiry(ttl);
                let now = StdInstant::now();
                let mut map = this.store.entries.lock().unwrap();
                match map.get(key.as_bytes().as_ref()) {
                    Some(e) if !e.expired(now) => Ok(false),
                    _ => {
                        map.insert(
                            key.as_bytes().to_vec(),
                            Entry {
                                value: rich,
                                expires_at,
                            },
                        );
                        Ok(true)
                    }
                }
            },
        );

        m.add_method(
            "increment",
            |_, this, (key, delta, ttl): (mlua::LuaString, Option<f64>, Option<f64>)| {
                let ttl = ttl_arg("increment", ttl)?;
                let delta = delta.unwrap_or(1.0);
                if delta.is_nan() {
                    return Err(LehuaError::msg("cache: increment: delta cannot be NaN").into());
                }
                let now = StdInstant::now();
                let mut map = this.store.entries.lock().unwrap();
                let existing = match map.get(key.as_bytes().as_ref()) {
                    Some(e) if !e.expired(now) => Some(e),
                    _ => None,
                };
                let (current, keep_expiry) = match existing {
                    Some(e) => match e.value {
                        RichValue::Int(i) => (i as f64, e.expires_at),
                        RichValue::Num(n) => (n, e.expires_at),
                        _ => {
                            return Err(LehuaError::msg(format!(
                                "cache: increment: key '{}' holds a non-number value",
                                key.to_string_lossy()
                            ))
                            .into())
                        }
                    },
                    None => (0.0, None),
                };
                let next = current + delta;
                let is_int = existing
                    .map(|e| matches!(e.value, RichValue::Int(_)))
                    .unwrap_or(true);
                let value = if is_int
                    && delta.fract() == 0.0
                    && next.abs() < i64::MAX as f64
                {
                    RichValue::Int(next as i64)
                } else {
                    RichValue::Num(next)
                };
                let expires_at = match (existing.is_some(), ttl) {
                    (true, None) => keep_expiry,
                    _ => this.store.expiry(ttl),
                };
                let result = match &value {
                    RichValue::Int(i) => int_value(*i),
                    RichValue::Num(n) => Value::Number(*n),
                    _ => Value::Nil,
                };
                map.insert(
                    key.as_bytes().to_vec(),
                    Entry { value, expires_at },
                );
                Ok(result)
            },
        );

        m.add_method("has", |_, this, key: mlua::LuaString| {
            let now = StdInstant::now();
            let map = this.store.entries.lock().unwrap();
            Ok(map
                .get(key.as_bytes().as_ref())
                .map(|e| !e.expired(now))
                .unwrap_or(false))
        });

        m.add_method("delete", |_, this, key: mlua::LuaString| {
            let now = StdInstant::now();
            let mut map = this.store.entries.lock().unwrap();
            match map.remove(key.as_bytes().as_ref()) {
                Some(e) => Ok(!e.expired(now)),
                None => Ok(false),
            }
        });

        m.add_method("ttl", |_, this, key: mlua::LuaString| {
            let now = StdInstant::now();
            let map = this.store.entries.lock().unwrap();
            match map.get(key.as_bytes().as_ref()) {
                Some(e) if !e.expired(now) => match e.expires_at {
                    Some(at) => Ok(Value::Number(at.duration_since(now).as_secs_f64())),
                    None => Ok(Value::Number(f64::INFINITY)),
                },
                _ => Ok(Value::Nil),
            }
        });

        m.add_method(
            "touch",
            |_, this, (key, ttl): (mlua::LuaString, Option<f64>)| {
                let ttl = ttl_arg("touch", ttl)?;
                let expires_at = this.store.expiry(ttl);
                let now = StdInstant::now();
                let mut map = this.store.entries.lock().unwrap();
                match map.get_mut(key.as_bytes().as_ref()) {
                    Some(e) if !e.expired(now) => {
                        e.expires_at = expires_at;
                        Ok(true)
                    }
                    _ => Ok(false),
                }
            },
        );

        m.add_method("keys", |lua, this, ()| {
            this.store.prune();
            let map = this.store.entries.lock().unwrap();
            let mut keys: Vec<&Vec<u8>> = map.keys().collect();
            keys.sort();
            let out = lua.create_table_with_capacity(keys.len(), 0)?;
            for (i, k) in (1usize..).zip(keys) {
                out.raw_seti(i, lua.create_string(k)?)?;
            }
            Ok(out)
        });

        m.add_method("count", |_, this, ()| {
            this.store.prune();
            Ok(this.store.entries.lock().unwrap().len())
        });

        m.add_method("clear", |_, this, ()| {
            this.store.entries.lock().unwrap().clear();
            Ok(())
        });

        m.add_method("name", |_, this, ()| Ok(this.name.clone()));

        m.add_method("stats", |lua, this, ()| {
            this.store.prune();
            let out = lua.create_table()?;
            out.set("hits", this.store.hits.load(Ordering::Relaxed))?;
            out.set("misses", this.store.misses.load(Ordering::Relaxed))?;
            out.set("count", this.store.entries.lock().unwrap().len())?;
            Ok(out)
        });

        m.add_meta_method(MetaMethod::Len, |_, this, ()| {
            this.store.prune();
            Ok(this.store.entries.lock().unwrap().len())
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("MemCache({})", this.name))
        });
    }
}

struct LocalSlot<T> {
    value: T,
    expires_at: Option<StdInstant>,
    used: u64,
}

struct LocalLru<T> {
    entries: RefCell<HashMap<Vec<u8>, LocalSlot<T>>>,
    order: RefCell<BTreeMap<u64, Vec<u8>>>,
    seq: Cell<u64>,
    capacity: usize,
    default_ttl: Option<f64>,
}

impl<T: Clone> LocalLru<T> {
    fn new(capacity: usize, default_ttl: Option<f64>) -> Self {
        LocalLru {
            entries: RefCell::new(HashMap::new()),
            order: RefCell::new(BTreeMap::new()),
            seq: Cell::new(0),
            capacity,
            default_ttl,
        }
    }

    fn next_seq(&self) -> u64 {
        let n = self.seq.get() + 1;
        self.seq.set(n);
        n
    }

    fn expiry(&self, ttl: Option<f64>) -> Option<StdInstant> {
        let secs = match ttl {
            Some(t) => Some(t),
            None => self.default_ttl,
        };
        match secs {
            Some(t) if t.is_finite() && t > 0.0 => {
                Some(StdInstant::now() + Duration::from_secs_f64(t.min(MAX_TTL_SECONDS)))
            }
            _ => None,
        }
    }

    fn insert(&self, key: Vec<u8>, value: T, ttl: Option<f64>) {
        let expires_at = self.expiry(ttl);
        let used = self.next_seq();
        let mut entries = self.entries.borrow_mut();
        let mut order = self.order.borrow_mut();
        if let Some(old) = entries.remove(&key) {
            order.remove(&old.used);
        }
        order.insert(used, key.clone());
        entries.insert(
            key,
            LocalSlot {
                value,
                expires_at,
                used,
            },
        );
        while entries.len() > self.capacity {
            let Some((_, oldest)) = order.pop_first() else {
                break;
            };
            entries.remove(&oldest);
        }
    }

    fn lookup(&self, key: &[u8], bump: bool) -> Option<T> {
        let now = StdInstant::now();
        let mut entries = self.entries.borrow_mut();
        let expired = match entries.get(key) {
            Some(slot) => slot.expires_at.map(|at| at <= now).unwrap_or(false),
            None => return None,
        };
        if expired {
            if let Some(slot) = entries.remove(key) {
                self.order.borrow_mut().remove(&slot.used);
            }
            return None;
        }
        let slot = entries.get_mut(key)?;
        if bump {
            let used = self.next_seq();
            let mut order = self.order.borrow_mut();
            order.remove(&slot.used);
            order.insert(used, key.to_vec());
            slot.used = used;
        }
        Some(slot.value.clone())
    }

    fn remove(&self, key: &[u8]) -> bool {
        let now = StdInstant::now();
        match self.entries.borrow_mut().remove(key) {
            Some(slot) => {
                self.order.borrow_mut().remove(&slot.used);
                slot.expires_at.map(|at| at > now).unwrap_or(true)
            }
            None => false,
        }
    }

    fn clear(&self) {
        self.entries.borrow_mut().clear();
        self.order.borrow_mut().clear();
    }

    fn prune(&self) {
        let now = StdInstant::now();
        let mut entries = self.entries.borrow_mut();
        let mut order = self.order.borrow_mut();
        entries.retain(|_, slot| {
            let keep = slot.expires_at.map(|at| at > now).unwrap_or(true);
            if !keep {
                order.remove(&slot.used);
            }
            keep
        });
    }

    fn keys(&self) -> Vec<Vec<u8>> {
        self.prune();
        let entries = self.entries.borrow();
        let mut keys: Vec<Vec<u8>> = entries.keys().cloned().collect();
        keys.sort();
        keys
    }

    fn count(&self) -> usize {
        self.prune();
        self.entries.borrow().len()
    }
}

pub struct LruCache {
    inner: LocalLru<Value>,
}

impl UserData for LruCache {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method(
            "set",
            |_, this, (key, value, ttl): (mlua::LuaString, Value, Option<f64>)| {
                let ttl = ttl_arg("set", ttl)?;
                this.inner.insert(key.as_bytes().to_vec(), value, ttl);
                Ok(())
            },
        );

        m.add_method("get", |_, this, key: mlua::LuaString| {
            Ok(this
                .inner
                .lookup(key.as_bytes().as_ref(), true)
                .unwrap_or(Value::Nil))
        });

        m.add_method("peek", |_, this, key: mlua::LuaString| {
            Ok(this
                .inner
                .lookup(key.as_bytes().as_ref(), false)
                .unwrap_or(Value::Nil))
        });

        m.add_method("has", |_, this, key: mlua::LuaString| {
            Ok(this.inner.lookup(key.as_bytes().as_ref(), false).is_some())
        });

        m.add_method("delete", |_, this, key: mlua::LuaString| {
            Ok(this.inner.remove(key.as_bytes().as_ref()))
        });

        m.add_method("clear", |_, this, ()| {
            this.inner.clear();
            Ok(())
        });

        m.add_method("keys", |lua, this, ()| {
            let keys = this.inner.keys();
            let out = lua.create_table_with_capacity(keys.len(), 0)?;
            for (i, k) in (1usize..).zip(keys) {
                out.raw_seti(i, lua.create_string(&k)?)?;
            }
            Ok(out)
        });

        m.add_method("count", |_, this, ()| Ok(this.inner.count()));

        m.add_method("capacity", |_, this, ()| Ok(this.inner.capacity));

        m.add_meta_method(MetaMethod::Len, |_, this, ()| Ok(this.inner.count()));

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("LruCache({})", this.inner.capacity))
        });
    }
}

fn encode_key(v: &RichValue, out: &mut Vec<u8>) {
    match v {
        RichValue::Nil => out.push(0),
        RichValue::Bool(b) => {
            out.push(1);
            out.push(*b as u8);
        }
        RichValue::Int(i) => {
            out.push(2);
            out.extend_from_slice(&i.to_le_bytes());
        }
        RichValue::Num(n) => {
            out.push(3);
            out.extend_from_slice(&n.to_bits().to_le_bytes());
        }
        RichValue::Str(b) => {
            out.push(4);
            out.extend_from_slice(&(b.len() as u64).to_le_bytes());
            out.extend_from_slice(b);
        }
        RichValue::Buffer(b) => {
            out.push(5);
            out.extend_from_slice(&(b.len() as u64).to_le_bytes());
            out.extend_from_slice(b);
        }
        RichValue::Vec3(x, y, z) => {
            out.push(6);
            out.extend_from_slice(&x.to_bits().to_le_bytes());
            out.extend_from_slice(&y.to_bits().to_le_bytes());
            out.extend_from_slice(&z.to_bits().to_le_bytes());
        }
        RichValue::Table { entries, meta } => {
            out.push(7);
            let mut encoded: Vec<(Vec<u8>, Vec<u8>)> = entries
                .iter()
                .map(|(k, v)| {
                    let mut kb = Vec::new();
                    let mut vb = Vec::new();
                    encode_key(k, &mut kb);
                    encode_key(v, &mut vb);
                    (kb, vb)
                })
                .collect();
            encoded.sort();
            out.extend_from_slice(&(encoded.len() as u64).to_le_bytes());
            for (kb, vb) in encoded {
                out.extend_from_slice(&kb);
                out.extend_from_slice(&vb);
            }
            if let Some(m) = meta {
                out.push(1);
                encode_key(m, out);
            } else {
                out.push(0);
            }
        }
        RichValue::Handle(name) => {
            out.push(8);
            out.extend_from_slice(&(name.len() as u64).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        #[cfg(feature = "lib-datetime")]
        RichValue::DateTime(micros) => {
            out.push(9);
            out.extend_from_slice(&micros.to_le_bytes());
        }
        #[cfg(feature = "lib-regex")]
        RichValue::Regex { pattern, flags } => {
            out.push(10);
            out.extend_from_slice(&(pattern.len() as u64).to_le_bytes());
            out.extend_from_slice(pattern.as_bytes());
            out.extend_from_slice(flags.as_bytes());
            out.push(0);
        }
        #[cfg(feature = "lib-sqlite")]
        RichValue::Sqlite(path) => {
            out.push(11);
            out.extend_from_slice(path.as_bytes());
            out.push(0);
        }
        #[cfg(feature = "lib-mongo")]
        RichValue::Mongo(r) => {
            out.push(12);
            for part in r.key_parts() {
                out.extend_from_slice(&(part.len() as u64).to_le_bytes());
                out.extend_from_slice(part.as_bytes());
            }
        }
        #[cfg(feature = "lib-canvas")]
        RichValue::Canvas {
            width,
            height,
            pixels,
        } => {
            out.push(13);
            out.extend_from_slice(&width.to_le_bytes());
            out.extend_from_slice(&height.to_le_bytes());
            out.extend_from_slice(pixels);
        }
    }
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    t.set(
        "mem",
        lua.create_function(|_, (first, second): (Value, Option<Table>)| {
            let (name, opts) = match first {
                Value::Nil => (None, second),
                Value::String(s) => (Some(s.to_str()?.to_string()), second),
                Value::Table(o) => (None, Some(o)),
                other => {
                    return Err(LehuaError::msg(format!(
                        "cache.mem expects an optional name and options table, got {}",
                        other.type_name()
                    ))
                    .into())
                }
            };
            let default_ttl = match &opts {
                Some(o) => match o.get::<Option<f64>>("ttl")? {
                    Some(ttl) if ttl.is_finite() && ttl > 0.0 => Some(ttl),
                    Some(_) => {
                        return Err(LehuaError::msg(
                            "cache.mem: ttl must be a positive number of seconds",
                        )
                        .into())
                    }
                    None => None,
                },
                None => None,
            };
            let name = name.unwrap_or_else(|| {
                format!("@anon-{}", ANON_COUNTER.fetch_add(1, Ordering::Relaxed))
            });
            let store = open_store(&name);
            if let Some(ttl) = default_ttl {
                *store.default_ttl.lock().unwrap() = Some(ttl);
            }
            Ok(MemCache { name, store })
        })?,
    )?;

    t.set(
        "lru",
        lua.create_function(|_, (capacity, opts): (f64, Option<Table>)| {
            if !capacity.is_finite() || capacity < 1.0 {
                return Err(
                    LehuaError::msg("cache.lru: capacity must be at least 1").into()
                );
            }
            let default_ttl = match &opts {
                Some(o) => match o.get::<Option<f64>>("ttl")? {
                    Some(ttl) if ttl.is_finite() && ttl > 0.0 => Some(ttl),
                    Some(_) => {
                        return Err(LehuaError::msg(
                            "cache.lru: ttl must be a positive number of seconds",
                        )
                        .into())
                    }
                    None => None,
                },
                None => None,
            };
            Ok(LruCache {
                inner: LocalLru::new(capacity as usize, default_ttl),
            })
        })?,
    )?;

    t.set(
        "memoize",
        lua.create_function(|lua, (func, opts): (Function, Option<Table>)| {
            let mut ttl = None;
            let mut capacity = 256usize;
            if let Some(o) = &opts {
                match o.get::<Option<f64>>("ttl")? {
                    Some(v) if v.is_finite() && v > 0.0 => ttl = Some(v),
                    Some(_) => {
                        return Err(LehuaError::msg(
                            "cache.memoize: ttl must be a positive number of seconds",
                        )
                        .into())
                    }
                    None => {}
                }
                match o.get::<Option<f64>>("capacity")? {
                    Some(v) if v.is_finite() && v >= 1.0 => capacity = v as usize,
                    Some(_) => {
                        return Err(LehuaError::msg(
                            "cache.memoize: capacity must be at least 1",
                        )
                        .into())
                    }
                    None => {}
                }
            }
            let state: Rc<LocalLru<Vec<Value>>> = Rc::new(LocalLru::new(capacity, ttl));
            lua.create_async_function(move |_, args: MultiValue| {
                let func = func.clone();
                let state = state.clone();
                async move {
                    let mut key = Vec::new();
                    for v in args.iter() {
                        let rich = RichValue::from_lua(v).map_err(|e| {
                            LehuaError::msg(format!(
                                "cache.memoize: argument is not cacheable: {e}"
                            ))
                        })?;
                        encode_key(&rich, &mut key);
                    }
                    if let Some(hit) = state.lookup(&key, true) {
                        return Ok(MultiValue::from_vec(hit));
                    }
                    let results = func.call_async::<MultiValue>(args).await?;
                    let stored: Vec<Value> = results.iter().cloned().collect();
                    state.insert(key, stored, None);
                    Ok(results)
                }
            })
        })?,
    )?;

    t.set(
        "list",
        lua.create_function(|lua, ()| {
            let map = stores().lock().unwrap();
            let mut names: Vec<String> = map.keys().cloned().collect();
            names.sort();
            let out = lua.create_table_with_capacity(names.len(), 0)?;
            for (i, n) in (1usize..).zip(names) {
                out.raw_seti(i, n)?;
            }
            Ok(out)
        })?,
    )?;

    t.set(
        "drop",
        lua.create_function(|_, name: String| {
            match stores().lock().unwrap().remove(&name) {
                Some(store) => {
                    store.entries.lock().unwrap().clear();
                    Ok(true)
                }
                None => Ok(false),
            }
        })?,
    )?;

    Ok(Value::Table(t))
}
