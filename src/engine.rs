use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use mlua::chunk::Compiler;
use mlua::thread::{AsyncThread, ThreadStatus};
use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Value};

use crate::dll;
use crate::error::{LehuaError, Result};
use crate::headers;
use crate::libs::{self, LibCtx};
use crate::parallel;
use crate::portable::PortableValue;
use crate::provider::ModuleProvider;
use crate::resolver::{Resolved, Resolver};
use crate::vpath;

pub struct Engine {
    pub provider: Arc<dyn ModuleProvider>,
    pub resolver: Arc<Resolver>,
    pub entry_id: String,
    pub flat_dirs: bool,
    #[allow(dead_code)]
    pub args: Vec<String>,
}

impl Engine {
    pub fn real_dir_of(&self, id: &str) -> PathBuf {
        let dir = vpath::dirname(id);
        if self.flat_dirs || dir.is_empty() {
            self.provider.base_dir().to_path_buf()
        } else {
            self.provider.base_dir().join(vpath::to_native(&dir))
        }
    }
    pub fn real_file_of(&self, id: &str) -> PathBuf {
        self.provider.base_dir().join(vpath::to_native(id))
    }

    pub fn call_chain(&self, lua: &Lua) -> Vec<String> {
        let loading = lua.app_data_ref::<LoadingMap>().map(|l| l.clone());
        let mut frames: Vec<String> = Vec::new();
        let mut level = 0;
        loop {
            let frame = lua.inspect_stack(level, |d| {
                let s = d.source();
                if s.what == "C" {
                    return None;
                }
                s.source.map(|src| src.into_owned())
            });
            match frame {
                None => break,
                Some(Some(src)) => {
                    let id = src.strip_prefix('@').unwrap_or(&src);
                    if self.provider.exists(id) {
                        if frames.last().map(|l| l != id).unwrap_or(true) {
                            frames.push(id.to_string());
                        }
                        if loading.as_ref().map(|l| l.is_loading(id)).unwrap_or(false) {
                            break;
                        }
                    }
                }
                Some(None) => {}
            }
            level += 1;
        }
        let mut chain: Vec<String> = Vec::new();
        for id in frames.into_iter().rev() {
            if !chain.contains(&id) {
                chain.push(id);
            }
        }
        chain
    }

    pub fn from_chain(&self, lua: &Lua, from_id: &str) -> Vec<String> {
        let mut chain = self.call_chain(lua);
        if !chain.iter().any(|c| c == from_id) {
            chain.push(from_id.to_string());
        }
        chain
    }
}

#[derive(Clone, Default)]
pub struct LoadingMap(Rc<RefCell<HashMap<String, (usize, Rc<tokio::sync::Notify>)>>>);

impl LoadingMap {
    fn is_loading(&self, id: &str) -> bool {
        self.0.borrow().contains_key(id)
    }
}

pub struct SchedGuard {
    sched: Rc<VmScheduler>,
}

impl SchedGuard {
    pub fn new(sched: &Rc<VmScheduler>) -> Self {
        sched.retain_task();
        SchedGuard {
            sched: sched.clone(),
        }
    }
}

impl Drop for SchedGuard {
    fn drop(&mut self) {
        self.sched.release_task();
    }
}

pub struct ParkSlot {
    args: RefCell<Option<MultiValue>>,
    wake: tokio::sync::Notify,
    guard: RefCell<Option<SchedGuard>>,
}

impl ParkSlot {
    pub async fn wait(&self) {
        self.wake.notified().await;
    }

    pub fn take(&self) -> (MultiValue, Option<SchedGuard>) {
        (
            self.args.borrow_mut().take().unwrap_or_default(),
            self.guard.borrow_mut().take(),
        )
    }
}

enum ThreadEntry {
    Busy,
    Parked(Rc<ParkSlot>),
}

#[derive(Clone, Default)]
pub struct ThreadControl(Rc<RefCell<HashMap<usize, ThreadEntry>>>);

impl ThreadControl {
    pub fn of(lua: &Lua) -> ThreadControl {
        match lua.app_data_ref::<ThreadControl>() {
            Some(c) => c.clone(),
            None => {
                let c = ThreadControl::default();
                lua.set_app_data(c.clone());
                c
            }
        }
    }

    pub fn set_busy(&self, key: usize) {
        self.0.borrow_mut().insert(key, ThreadEntry::Busy);
    }

    pub fn remove(&self, key: usize) {
        self.0.borrow_mut().remove(&key);
    }

    pub fn park(&self, key: usize) -> Rc<ParkSlot> {
        let slot = Rc::new(ParkSlot {
            args: RefCell::new(None),
            wake: tokio::sync::Notify::new(),
            guard: RefCell::new(None),
        });
        self.0
            .borrow_mut()
            .insert(key, ThreadEntry::Parked(slot.clone()));
        slot
    }

    pub fn parked(&self, key: usize) -> Option<Rc<ParkSlot>> {
        match self.0.borrow().get(&key) {
            Some(ThreadEntry::Parked(slot)) => Some(slot.clone()),
            _ => None,
        }
    }

    pub fn is_busy(&self, key: usize) -> bool {
        matches!(self.0.borrow().get(&key), Some(ThreadEntry::Busy))
    }

    pub fn wake(&self, key: usize, sched: &Rc<VmScheduler>, args: MultiValue) -> bool {
        let Some(slot) = self.parked(key) else {
            return false;
        };
        *slot.args.borrow_mut() = Some(args);
        *slot.guard.borrow_mut() = Some(SchedGuard::new(sched));
        self.set_busy(key);
        slot.wake.notify_one();
        true
    }
}

pub fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

pub fn panic_error(payload: Box<dyn Any + Send>) -> mlua::Error {
    LehuaError::msg(format!("panic: {}", panic_message(payload.as_ref()))).into()
}

fn env_enabled(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => !v.is_empty() && v != "0",
        Err(_) => false,
    }
}

pub fn install_panic_hook() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if env_enabled("RUST_BACKTRACE") || env_enabled("LEHUA_PANIC_TRACE") {
                default(info);
            }
        }));
    });
}

pub async fn catch_panics<T>(fut: impl Future<Output = mlua::Result<T>>) -> mlua::Result<T> {
    let mut fut = Box::pin(fut);
    std::future::poll_fn(
        |cx| match catch_unwind(AssertUnwindSafe(|| fut.as_mut().poll(cx))) {
            Ok(poll) => poll,
            Err(payload) => Poll::Ready(Err(panic_error(payload))),
        },
    )
    .await
}

pub async fn thread_step(
    stream: &mut Pin<Box<AsyncThread<MultiValue>>>,
) -> Option<mlua::Result<MultiValue>> {
    std::future::poll_fn(|cx| {
        match catch_unwind(AssertUnwindSafe(|| {
            futures_core::Stream::poll_next(stream.as_mut(), cx)
        })) {
            Ok(poll) => poll,
            Err(payload) => Poll::Ready(Some(Err(panic_error(payload)))),
        }
    })
    .await
}

fn install_resume_hook(
    lua: &Lua,
    control: ThreadControl,
    sched: Rc<VmScheduler>,
) -> mlua::Result<()> {
    let co: mlua::Table = lua.globals().get("coroutine")?;
    let orig: Function = co.get("resume")?;
    let wrapped = lua.create_function(move |lua, mut args: MultiValue| {
        let target = match args.iter().next() {
            Some(Value::Thread(t)) => Some(t.clone()),
            _ => None,
        };
        if let Some(t) = target {
            let key = t.to_pointer() as usize;
            if control.parked(key).is_some() {
                args.pop_front();
                control.wake(key, &sched, args);
                return true.into_lua_multi(lua);
            }
            if control.is_busy(key) {
                return (
                    false,
                    "cannot resume a scheduler-managed thread unless it is suspended by coroutine.yield",
                )
                    .into_lua_multi(lua);
            }
        }
        orig.call::<MultiValue>(args)
    })?;
    co.set("resume", wrapped)
}

pub struct VmContext {
    pub engine: Arc<Engine>,
    pub cache: RefCell<HashMap<String, Value>>,
    pub loading: LoadingMap,
    pub waiting: RefCell<HashMap<usize, String>>,
    pub dlls: Rc<RefCell<HashMap<String, Arc<libloading::Library>>>>,
    pub sched: Rc<VmScheduler>,
}

impl VmContext {
    fn new(engine: Arc<Engine>) -> Rc<Self> {
        Rc::new(VmContext {
            engine,
            cache: RefCell::new(HashMap::new()),
            loading: LoadingMap::default(),
            waiting: RefCell::new(HashMap::new()),
            dlls: Rc::new(RefCell::new(HashMap::new())),
            sched: Rc::new(VmScheduler::default()),
        })
    }
}

#[derive(Default)]
pub struct VmScheduler {
    pub heartbeat: RefCell<Vec<(String, Function)>>,
    pub close: RefCell<Vec<Function>>,
    pub exit_code: Cell<i32>,
    tasks: Cell<usize>,
    idle: tokio::sync::Notify,
    heartbeats_live: Cell<bool>,
}

#[allow(dead_code)]
impl VmScheduler {
    pub fn bind_heartbeat(&self, name: String, func: Function) {
        let mut hb = self.heartbeat.borrow_mut();
        if let Some(slot) = hb.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = func;
        } else {
            hb.push((name, func));
        }
    }

    pub fn unbind_heartbeat(&self, name: &str) -> bool {
        let mut hb = self.heartbeat.borrow_mut();
        let before = hb.len();
        hb.retain(|(n, _)| n != name);
        hb.len() != before
    }

    pub fn retain_task(&self) {
        self.tasks.set(self.tasks.get() + 1);
    }

    pub fn release_task(&self) {
        let n = self.tasks.get().saturating_sub(1);
        self.tasks.set(n);
        if n == 0 {
            self.idle.notify_waiters();
        }
    }

    pub async fn wait_idle(&self) {
        while self.tasks.get() > 0 {
            let notified = self.idle.notified();
            if self.tasks.get() == 0 {
                break;
            }
            notified.await;
        }
    }

    pub async fn wait_unresumable(&self) {
        loop {
            self.wait_idle().await;
            if !self.heartbeats_live.get() || self.heartbeat.borrow().is_empty() {
                return;
            }
            self.idle.notified().await;
        }
    }

    pub fn run_close(&self) {
        let handlers: Vec<Function> = std::mem::take(&mut self.close.borrow_mut());
        let code = self.exit_code.get();
        for f in handlers {
            match catch_unwind(AssertUnwindSafe(|| f.call::<()>(code))) {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    eprintln!("lehua: close handler error: {}", crate::error::pretty(&e))
                }
                Err(payload) => eprintln!(
                    "lehua: close handler error: {}",
                    crate::error::pretty(panic_error(payload))
                ),
            }
        }
    }
}

pub fn make_vm(engine: Arc<Engine>) -> Result<(Lua, Rc<VmContext>)> {
    let lua = Lua::new();
    lua.set_compiler(
        Compiler::new()
            .set_optimization_level(2)
            .set_type_info_level(1),
    );
    lua.enable_jit(true);
    install_pretty_print(&lua)?;
    let ctx = VmContext::new(engine);
    lua.set_app_data(ctx.loading.clone());
    let control = ThreadControl::of(&lua);
    install_resume_hook(&lua, control, ctx.sched.clone())?;
    crate::messenger::install(&lua, ctx.sched.clone())?;
    crate::class::install(&lua)?;
    Ok((lua, ctx))
}

const PRINT_MAX_DEPTH: usize = 6;

#[derive(Default)]
pub struct PrintCapture(pub RefCell<Option<Vec<u8>>>);

fn install_pretty_print(lua: &Lua) -> mlua::Result<()> {
    lua.set_app_data(PrintCapture::default());
    let tostring: Function = lua.globals().get("tostring")?;
    let print_fn = lua.create_function(move |lua, args: MultiValue| {
        use std::io::{IsTerminal, Write};
        let style = std::io::stdout().is_terminal();
        let mut out: Vec<u8> = Vec::new();
        for (i, v) in args.iter().enumerate() {
            if i > 0 {
                out.push(b' ');
            }
            format_value(&tostring, v, &mut out, 0, &mut Vec::new(), style)?;
        }
        if let Some(cap) = lua.app_data_ref::<PrintCapture>() {
            if let Some(buf) = cap.0.borrow_mut().as_mut() {
                buf.extend_from_slice(&out);
                return Ok(());
            }
        }
        out.push(b'\n');
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
        Ok(())
    })?;
    lua.globals().set("print", print_fn)
}

fn write_styled(out: &mut Vec<u8>, style: bool, code: &str, text: &[u8]) {
    if style {
        out.extend_from_slice(format!("\u{1b}[{code}m").as_bytes());
        out.extend_from_slice(text);
        out.extend_from_slice(b"\x1b[0m");
    } else {
        out.extend_from_slice(text);
    }
}

fn format_value(
    tostring: &Function,
    v: &Value,
    out: &mut Vec<u8>,
    depth: usize,
    path: &mut Vec<usize>,
    style: bool,
) -> mlua::Result<()> {
    match v {
        Value::Nil => write_styled(out, style, "90", b"nil"),
        Value::Boolean(b) => {
            write_styled(out, style, "94", if *b { b"true" } else { b"false" })
        }
        Value::Integer(i) => write_styled(out, style, "33", i.to_string().as_bytes()),
        Value::Number(n) => write_styled(out, style, "33", n.to_string().as_bytes()),
        Value::String(s) => {
            if depth == 0 {
                out.extend_from_slice(&s.as_bytes());
            } else {
                quote_string(&s.as_bytes(), out);
            }
        }
        Value::Table(t) => {
            if let Some(text) = toconsole_bytes(tostring, v)? {
                out.extend_from_slice(&text);
            } else if has_custom_tostring(t) {
                let text = tostring.call::<mlua::LuaString>(v.clone())?;
                out.extend_from_slice(&text.as_bytes());
            } else {
                format_table(tostring, t, out, depth, path, style)?
            }
        }
        Value::UserData(_) => {
            let text = tostring.call::<mlua::LuaString>(v.clone())?;
            if style {
                out.extend_from_slice(b"\x1b[1;34m[");
                out.extend_from_slice(&text.as_bytes());
                out.extend_from_slice(b"]\x1b[0m");
            } else {
                out.push(b'[');
                out.extend_from_slice(&text.as_bytes());
                out.push(b']');
            }
        }
        Value::LightUserData(_) if *v == Value::NULL => write_styled(out, style, "90", b"null"),
        other => {
            let text = tostring.call::<mlua::LuaString>(other.clone())?;
            write_styled(out, style, "35", &text.as_bytes());
        }
    }
    Ok(())
}

fn has_custom_tostring(t: &mlua::Table) -> bool {
    match t.metatable() {
        Some(mt) => matches!(mt.raw_get::<Value>("__tostring"), Ok(v) if !v.is_nil()),
        None => false,
    }
}

fn console_field(t: &mlua::Table) -> mlua::Result<Value> {
    let direct: Value = t.raw_get("__toconsole")?;
    if !direct.is_nil() {
        return Ok(direct);
    }
    match t.metatable() {
        Some(mt) => mt.raw_get("__toconsole"),
        None => Ok(Value::Nil),
    }
}

pub fn toconsole_bytes(tostring: &Function, v: &Value) -> mlua::Result<Option<Vec<u8>>> {
    let t = match v {
        Value::Table(t) => t,
        _ => return Ok(None),
    };
    match console_field(t)? {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s.as_bytes().to_vec())),
        Value::Function(f) => {
            let produced = f.call::<Value>(v.clone())?;
            let text = match produced {
                Value::String(s) => s.as_bytes().to_vec(),
                other => tostring.call::<mlua::LuaString>(other)?.as_bytes().to_vec(),
            };
            Ok(Some(text))
        }
        other => Ok(Some(tostring.call::<mlua::LuaString>(other)?.as_bytes().to_vec())),
    }
}

fn format_table(
    tostring: &Function,
    t: &mlua::Table,
    out: &mut Vec<u8>,
    depth: usize,
    path: &mut Vec<usize>,
    style: bool,
) -> mlua::Result<()> {
    let ptr = t.to_pointer() as usize;
    if path.contains(&ptr) || depth >= PRINT_MAX_DEPTH {
        out.extend_from_slice(b"{...}");
        return Ok(());
    }
    let len = t.raw_len();
    let mut named: Vec<(Vec<u8>, Value, Value)> = Vec::new();
    for entry in t.pairs::<Value, Value>() {
        let (k, v) = entry?;
        if let Value::Integer(i) = k {
            if i >= 1 && (i as usize) <= len {
                continue;
            }
        }
        let mut sort_key = Vec::new();
        match &k {
            Value::String(s) => sort_key.extend_from_slice(&s.as_bytes()),
            other => sort_key.extend_from_slice(
                tostring.call::<mlua::LuaString>(other.clone())?.as_bytes().as_ref(),
            ),
        }
        named.push((sort_key, k, v));
    }
    if len == 0 && named.is_empty() {
        out.extend_from_slice(b"{}");
        return Ok(());
    }
    named.sort_by(|a, b| a.0.cmp(&b.0));
    path.push(ptr);
    out.extend_from_slice(b"{\n");
    let pad = |out: &mut Vec<u8>, levels: usize| {
        for _ in 0..levels {
            out.extend_from_slice(b"    ");
        }
    };
    for i in 1..=len {
        pad(out, depth + 1);
        let item: Value = t.raw_get(i)?;
        format_value(tostring, &item, out, depth + 1, path, style)?;
        out.extend_from_slice(b",\n");
    }
    for (sort_key, k, v) in named {
        pad(out, depth + 1);
        if matches!(&k, Value::String(_)) && is_identifier(&sort_key) {
            write_styled(out, style, "36", &sort_key);
        } else {
            out.push(b'[');
            format_value(tostring, &k, out, depth + 1, path, style)?;
            out.push(b']');
        }
        out.extend_from_slice(b" = ");
        format_value(tostring, &v, out, depth + 1, path, style)?;
        out.extend_from_slice(b",\n");
    }
    pad(out, depth);
    out.push(b'}');
    path.pop();
    Ok(())
}

fn is_identifier(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes[0].is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || b == b'_')
}

fn quote_string(bytes: &[u8], out: &mut Vec<u8>) {
    out.push(b'"');
    for &b in bytes {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x20..=0x7e | 0x80..=0xff => out.push(b),
            other => out.extend_from_slice(format!("\\{other}").as_bytes()),
        }
    }
    out.push(b'"');
}

pub async fn run_entry(
    lua: Lua,
    ctx: Rc<VmContext>,
    entry_id: &str,
    channel: Option<Value>,
    args: Vec<PortableValue>,
) -> mlua::Result<Value> {
    if let Some(chan) = channel {
        lua.globals().set("channel", chan)?;
    }
    let extra: Vec<Value> = args
        .into_iter()
        .map(|a| a.into_lua(&lua))
        .collect::<mlua::Result<_>>()?;

    let result = load_module(&lua, &ctx, entry_id, extra).await;

    match result {
        Ok(v) => {
            tokio::join!(heartbeat_loop(&ctx), ctx.sched.wait_idle());
            ctx.sched.run_close();
            Ok(v)
        }
        Err(e) => {
            ctx.sched.exit_code.set(1);
            ctx.sched.run_close();
            Err(e)
        }
    }
}

async fn heartbeat_loop(ctx: &Rc<VmContext>) {
    if ctx.sched.heartbeat.borrow().is_empty() {
        return;
    }
    ctx.sched.heartbeats_live.set(true);
    let frame = Duration::from_secs_f64(1.0 / 60.0);
    let mut last = Instant::now();
    loop {
        tokio::time::sleep(frame).await;
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64();
        last = now;

        let funcs: Vec<Function> = ctx
            .sched
            .heartbeat
            .borrow()
            .iter()
            .map(|(_, f)| f.clone())
            .collect();
        if funcs.is_empty() {
            ctx.sched.heartbeats_live.set(false);
            return;
        }
        for f in funcs {
            if let Err(e) = catch_panics(f.call_async::<()>(dt)).await {
                eprintln!("lehua: heartbeat error: {}", crate::error::pretty(&e));
            }
        }
    }
}

async fn require_impl(
    lua: &Lua,
    ctx: &Rc<VmContext>,
    from_id: &str,
    request: &str,
) -> mlua::Result<Value> {
    let chain = ctx.engine.from_chain(lua, from_id);
    let resolved = ctx
        .engine
        .resolver
        .resolve_chain(&chain, from_id, request, ctx.engine.provider.as_ref())?;

    match resolved {
        Resolved::Builtin(name) => builtin_value(lua, ctx, from_id, &name),
        Resolved::Module(id) => load_module(lua, ctx, &id, Vec::new()).await,
    }
}

fn builtin_value(lua: &Lua, ctx: &Rc<VmContext>, from_id: &str, name: &str) -> mlua::Result<Value> {
    let from_dir = ctx.engine.real_dir_of(from_id);
    let key = format!("@builtin:{name}:{}", vpath::dirname(from_id));
    if let Some(v) = ctx.cache.borrow().get(&key) {
        return Ok(v.clone());
    }
    let lib_ctx = LibCtx {
        lua,
        engine: &ctx.engine,
        real_dir: from_dir,
        sched: ctx.sched.clone(),
        from_id: from_id.to_string(),
        dlls: ctx.dlls.clone(),
    };
    let value = libs::build(name, &lib_ctx)?;
    ctx.cache.borrow_mut().insert(key, value.clone());
    Ok(value)
}

struct LoadingGuard {
    ctx: Rc<VmContext>,
    id: String,
    notify: Rc<tokio::sync::Notify>,
}

impl Drop for LoadingGuard {
    fn drop(&mut self) {
        self.ctx.loading.0.borrow_mut().remove(&self.id);
        self.notify.notify_waiters();
    }
}

struct WaitingGuard {
    ctx: Rc<VmContext>,
    thread: usize,
}

impl Drop for WaitingGuard {
    fn drop(&mut self) {
        self.ctx.waiting.borrow_mut().remove(&self.thread);
    }
}

fn detect_require_cycle(ctx: &Rc<VmContext>, id: &str, me: usize) -> bool {
    let loading = ctx.loading.0.borrow();
    let waiting = ctx.waiting.borrow();
    let mut cur = id.to_string();
    for _ in 0..1024 {
        let Some((loader, _)) = loading.get(&cur) else {
            return false;
        };
        if *loader == me {
            return true;
        }
        match waiting.get(loader) {
            Some(next) => cur = next.clone(),
            None => return false,
        }
    }
    true
}

async fn load_module(
    lua: &Lua,
    ctx: &Rc<VmContext>,
    id: &str,
    extra_varargs: Vec<Value>,
) -> mlua::Result<Value> {
    let me = lua.current_thread().to_pointer() as usize;
    loop {
        if let Some(v) = ctx.cache.borrow().get(id) {
            return Ok(v.clone());
        }
        let pending = ctx.loading.0.borrow().get(id).cloned();
        match pending {
            Some((_, notify)) => {
                if detect_require_cycle(ctx, id, me) {
                    return Err(LehuaError::CircularRequire(id.to_string()).into());
                }
                ctx.waiting.borrow_mut().insert(me, id.to_string());
                let waiting_guard = WaitingGuard {
                    ctx: ctx.clone(),
                    thread: me,
                };
                notify.notified().await;
                drop(waiting_guard);
            }
            None => break,
        }
    }

    let notify = Rc::new(tokio::sync::Notify::new());
    ctx.loading
        .0
        .borrow_mut()
        .insert(id.to_string(), (me, notify.clone()));
    let guard = LoadingGuard {
        ctx: ctx.clone(),
        id: id.to_string(),
        notify,
    };

    let result = execute_module(lua, ctx, id, extra_varargs).await;

    drop(guard);
    let value = result?;
    ctx.cache.borrow_mut().insert(id.to_string(), value.clone());
    Ok(value)
}

async fn execute_module(
    lua: &Lua,
    ctx: &Rc<VmContext>,
    id: &str,
    extra_varargs: Vec<Value>,
) -> mlua::Result<Value> {
    let source = ctx.engine.provider.read(id)?;
    let directives = headers::parse(&source);

    let mut include_names: Vec<String> = Vec::new();
    for n in &directives.includes {
        if n == "all" || n == "*" {
            for k in libs::KNOWN {
                let k = (*k).to_string();
                if !include_names.contains(&k) {
                    include_names.push(k);
                }
            }
        } else if ctx.engine.resolver.known_builtins.contains(n) && !include_names.contains(n) {
            include_names.push(n.clone());
        }
    }
    let inject_names: Vec<String> = directives
        .injects
        .iter()
        .map(|d| headers::inject_global_name(d))
        .collect();

    let func = compile_module(lua, id, &source, &include_names, &inject_names)?;

    let require_fn = make_require(lua, ctx.clone(), id)?;
    let parallel_fn = parallel::make_parallel(lua, ctx.engine.clone(), id)?;
    let dirname = ctx.engine.real_dir_of(id).to_string_lossy().into_owned();
    let filename = ctx.engine.real_file_of(id).to_string_lossy().into_owned();

    let mut args: Vec<Value> = vec![
        Value::Function(require_fn),
        Value::Function(parallel_fn),
        Value::String(lua.create_string(&dirname)?),
        Value::String(lua.create_string(&filename)?),
    ];
    for name in &include_names {
        args.push(builtin_value(lua, ctx, id, name)?);
    }
    for entry in directives.injects.iter() {
        let g = dll::make_dll_global(lua, ctx, id, entry)?;
        args.push(g);
    }
    args.extend(extra_varargs);

    run_module_thread(lua, ctx, func, MultiValue::from_vec(args)).await
}

async fn run_module_thread(
    lua: &Lua,
    ctx: &Rc<VmContext>,
    func: Function,
    args: MultiValue,
) -> mlua::Result<Value> {
    let control = ThreadControl::of(lua);
    let thread = lua.create_thread(func)?;
    let key = thread.to_pointer() as usize;
    control.set_busy(key);
    let mut current_args = args;
    let mut held: Option<SchedGuard> = None;
    let result = loop {
        let mut stream = match thread.clone().into_async::<MultiValue>(current_args) {
            Ok(s) => Box::pin(s),
            Err(e) => break Err(e),
        };
        match thread_step(&mut stream).await {
            Some(Ok(vals)) => {
                if thread.status() != ThreadStatus::Resumable {
                    break Ok(vals.into_iter().next().unwrap_or(Value::Nil));
                }
                drop(stream);
                let slot = control.park(key);
                held.take();
                tokio::select! {
                    _ = slot.wait() => {
                        let (args, guard) = slot.take();
                        current_args = args;
                        held = guard;
                        control.set_busy(key);
                    }
                    _ = ctx.sched.wait_unresumable() => break Ok(Value::Nil),
                }
            }
            Some(Err(e)) => break Err(e),
            None => break Ok(Value::Nil),
        }
    };
    control.remove(key);
    drop(held);
    result
}

fn compile_module(
    lua: &Lua,
    id: &str,
    source: &str,
    includes: &[String],
    injects: &[String],
) -> mlua::Result<Function> {
    let mut params = String::from("require, parallel, __dirname, __filename");
    for name in includes.iter().chain(injects.iter()) {
        params.push_str(", ");
        params.push_str(name);
    }
    let source = strip_shebang(source);
    let wrapped = format!("return function({params}, ...) {source}\nend");
    lua.load(&wrapped).set_name(format!("@{id}")).eval::<Function>()
}

fn strip_shebang(source: &str) -> std::borrow::Cow<'_, str> {
    if source.starts_with("#!") {
        match source.find('\n') {
            Some(nl) => std::borrow::Cow::Owned(source[nl..].to_string()),
            None => std::borrow::Cow::Borrowed(""),
        }
    } else {
        std::borrow::Cow::Borrowed(source)
    }
}

fn make_require(lua: &Lua, ctx: Rc<VmContext>, from_id: &str) -> mlua::Result<Function> {
    let from_id = from_id.to_string();
    lua.create_async_function(move |lua, request: String| {
        let ctx = ctx.clone();
        let from_id = from_id.clone();
        async move { require_impl(&lua, &ctx, &from_id, &request).await }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(lua: &Lua, src: &str) -> String {
        let tostring: Function = lua.globals().get("tostring").unwrap();
        let value: Value = lua.load(src).eval().unwrap();
        let mut out = Vec::new();
        format_value(&tostring, &value, &mut out, 0, &mut Vec::new(), false).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn toconsole_from_metatable_function() {
        let lua = Lua::new();
        assert_eq!(
            render(&lua, r#"return setmetatable({}, { __toconsole = function() return "mt-fn" end })"#),
            "mt-fn"
        );
    }

    #[test]
    fn toconsole_from_metatable_string() {
        let lua = Lua::new();
        assert_eq!(
            render(&lua, r#"return setmetatable({}, { __toconsole = "mt-str" })"#),
            "mt-str"
        );
    }

    #[test]
    fn toconsole_from_direct_field() {
        let lua = Lua::new();
        assert_eq!(
            render(&lua, r#"return { __toconsole = "direct" }"#),
            "direct"
        );
        assert_eq!(
            render(&lua, r#"local t = {} t.__toconsole = function() return "direct-fn" end return t"#),
            "direct-fn"
        );
    }

    #[test]
    fn plain_table_without_toconsole_formats_normally() {
        let lua = Lua::new();
        assert_eq!(render(&lua, r#"return {}"#), "{}");
    }
}
