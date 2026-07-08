use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mlua::chunk::Compiler;
use mlua::{Function, Lua, MultiValue, Value};

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
}

pub struct VmContext {
    pub engine: Arc<Engine>,
    pub cache: RefCell<HashMap<String, Value>>,
    pub loading: RefCell<HashSet<String>>,
    pub dlls: Rc<RefCell<HashMap<String, Arc<libloading::Library>>>>,
    pub sched: Rc<VmScheduler>,
}

impl VmContext {
    fn new(engine: Arc<Engine>) -> Rc<Self> {
        Rc::new(VmContext {
            engine,
            cache: RefCell::new(HashMap::new()),
            loading: RefCell::new(HashSet::new()),
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
}

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

    pub fn run_close(&self) {
        let handlers: Vec<Function> = std::mem::take(&mut self.close.borrow_mut());
        let code = self.exit_code.get();
        for f in handlers {
            if let Err(e) = f.call::<()>(code) {
                eprintln!("lehua: close handler error: {e}");
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
    let ctx = VmContext::new(engine);
    Ok((lua, ctx))
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
            heartbeat_loop(&ctx).await;
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
            return;
        }
        for f in funcs {
            if let Err(e) = f.call_async::<()>(dt).await {
                eprintln!("lehua: heartbeat error: {e}");
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
    let resolved = ctx
        .engine
        .resolver
        .resolve(from_id, request, ctx.engine.provider.as_ref())?;

    match resolved {
        Resolved::Builtin(name) => builtin_value(lua, ctx, from_id, &name),
        Resolved::Module(id) => load_module(lua, ctx, &id, Vec::new()).await,
    }
}

fn builtin_value(lua: &Lua, ctx: &Rc<VmContext>, from_id: &str, name: &str) -> mlua::Result<Value> {
    let from_dir = ctx.engine.real_dir_of(from_id);
    let key = format!("@builtin:{name}:{}", from_dir.display());
    if let Some(v) = ctx.cache.borrow().get(&key) {
        return Ok(v.clone());
    }
    let lib_ctx = LibCtx {
        lua,
        engine: &ctx.engine,
        real_dir: from_dir,
        sched: ctx.sched.clone(),
    };
    let value = libs::build(name, &lib_ctx)?;
    ctx.cache.borrow_mut().insert(key, value.clone());
    Ok(value)
}

async fn load_module(
    lua: &Lua,
    ctx: &Rc<VmContext>,
    id: &str,
    extra_varargs: Vec<Value>,
) -> mlua::Result<Value> {
    if let Some(v) = ctx.cache.borrow().get(id) {
        return Ok(v.clone());
    }
    if !ctx.loading.borrow_mut().insert(id.to_string()) {
        return Err(LehuaError::CircularRequire(id.to_string()).into());
    }

    let result = execute_module(lua, ctx, id, extra_varargs).await;

    ctx.loading.borrow_mut().remove(id);
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

    func.call_async::<Value>(MultiValue::from_vec(args)).await
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
    lua.load(&wrapped).set_name(id).eval::<Function>()
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
