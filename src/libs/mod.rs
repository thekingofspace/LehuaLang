pub mod fs;
pub mod process;

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use mlua::{Lua, Value};

use crate::engine::{Engine, VmScheduler};
use crate::error::LehuaError;
use crate::vpath;

pub struct LibCtx<'a> {
    pub lua: &'a Lua,
    pub engine: &'a Arc<Engine>,
    pub real_dir: PathBuf,
    pub sched: Rc<VmScheduler>,
}

pub const KNOWN: &[&str] = &["fs", "process"];

pub fn build(name: &str, ctx: &LibCtx) -> mlua::Result<Value> {
    match name {
        "fs" => fs::build(ctx),
        "process" => process::build(ctx),
        other => Err(LehuaError::msg(format!("unknown built-in library '{other}'")).into()),
    }
}

pub struct PathScope {
    base: PathBuf,
    root: PathBuf,
    aliases: BTreeMap<String, String>,
}

impl PathScope {
    pub fn new(ctx: &LibCtx) -> Rc<Self> {
        Rc::new(PathScope {
            base: ctx.real_dir.clone(),
            root: ctx.engine.provider.base_dir().to_path_buf(),
            aliases: ctx.engine.resolver.aliases.clone(),
        })
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    pub fn resolve(&self, p: &str) -> mlua::Result<PathBuf> {
        if let Some(rest) = p.strip_prefix('@') {
            let (name, sub) = match rest.find(['/', '\\']) {
                Some(i) => (&rest[..i], &rest[i + 1..]),
                None => (rest, ""),
            };
            let dir = if name == "self" {
                self.base.clone()
            } else if let Some(a) = self.aliases.get(name) {
                self.root.join(vpath::to_native(a))
            } else {
                return Err(LehuaError::msg(format!("unknown path alias '@{name}'")).into());
            };
            let joined = if sub.is_empty() { dir } else { dir.join(sub) };
            return Ok(normalize(&joined));
        }
        let pp = Path::new(p);
        let joined = if pp.is_absolute() {
            pp.to_path_buf()
        } else {
            self.base.join(pp)
        };
        Ok(normalize(&joined))
    }
}

pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut normals: Vec<std::ffi::OsString> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if normals.last().map(|s| s.as_os_str() != "..").unwrap_or(false) {
                    normals.pop();
                } else {
                    normals.push("..".into());
                }
            }
            Component::Normal(n) => normals.push(n.to_os_string()),
        }
    }
    for n in normals {
        out.push(n);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}
