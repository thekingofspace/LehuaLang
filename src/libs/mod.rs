#[cfg(feature = "lib-archive")]
pub mod archive;
#[cfg(feature = "lib-cache")]
pub mod cache;
#[cfg(feature = "lib-canvas")]
pub mod canvas;
#[cfg(feature = "lib-cryptography")]
pub mod cryptography;
#[cfg(feature = "lib-datetime")]
pub mod datetime;
pub mod foreign;
#[cfg(feature = "lib-fs")]
pub mod fs;
#[cfg(feature = "lib-luau")]
pub mod luau;
#[cfg(feature = "lib-mongo")]
pub mod mongo;
#[cfg(feature = "lib-net")]
pub mod net;
#[cfg(feature = "lib-process")]
pub mod process;
#[cfg(feature = "lib-random")]
pub mod random;
#[cfg(feature = "lib-regex")]
pub mod regex;
#[cfg(feature = "lib-semver")]
pub mod semver;
#[cfg(feature = "lib-serde")]
pub mod serde;
#[cfg(feature = "lib-sqlite")]
pub mod sqlite;
#[cfg(feature = "lib-stdio")]
pub mod stdio;
#[cfg(feature = "lib-task")]
pub mod task;
#[cfg(feature = "lib-url")]
pub mod url;

use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use mlua::{Lua, Value};

use crate::engine::{Engine, VmScheduler};
use crate::error::LehuaError;
use crate::vpath;

#[allow(dead_code)]
pub struct LibCtx<'a> {
    pub lua: &'a Lua,
    pub engine: &'a Arc<Engine>,
    pub real_dir: PathBuf,
    pub sched: Rc<VmScheduler>,
    pub from_id: String,
    pub dlls: Rc<std::cell::RefCell<std::collections::HashMap<String, std::sync::Arc<libloading::Library>>>>,
}

pub const KNOWN: &[&str] = &[
    #[cfg(feature = "lib-fs")]
    "fs",
    #[cfg(feature = "lib-process")]
    "process",
    #[cfg(feature = "lib-serde")]
    "serde",
    #[cfg(feature = "lib-cryptography")]
    "cryptography",
    #[cfg(feature = "lib-datetime")]
    "datetime",
    #[cfg(feature = "lib-regex")]
    "regex",
    #[cfg(feature = "lib-stdio")]
    "stdio",
    #[cfg(feature = "lib-luau")]
    "luau",
    #[cfg(feature = "lib-url")]
    "url",
    #[cfg(feature = "lib-semver")]
    "semver",
    #[cfg(feature = "lib-archive")]
    "archive",
    #[cfg(feature = "lib-sqlite")]
    "sqlite",
    #[cfg(feature = "lib-mongo")]
    "mongo",
    #[cfg(feature = "lib-net")]
    "net",
    #[cfg(feature = "lib-random")]
    "random",
    #[cfg(feature = "lib-task")]
    "task",
    #[cfg(feature = "lib-canvas")]
    "canvas",
    #[cfg(feature = "lib-cache")]
    "cache",
    "dll",
];

pub fn build(name: &str, ctx: &LibCtx) -> mlua::Result<Value> {
    let _ = ctx;
    match name {
        #[cfg(feature = "lib-fs")]
        "fs" => self::fs::build(ctx),
        #[cfg(feature = "lib-process")]
        "process" => self::process::build(ctx),
        #[cfg(feature = "lib-serde")]
        "serde" => self::serde::build(ctx),
        #[cfg(feature = "lib-cryptography")]
        "cryptography" => self::cryptography::build(ctx),
        #[cfg(feature = "lib-datetime")]
        "datetime" => self::datetime::build(ctx),
        #[cfg(feature = "lib-regex")]
        "regex" => self::regex::build(ctx),
        #[cfg(feature = "lib-stdio")]
        "stdio" => self::stdio::build(ctx),
        #[cfg(feature = "lib-luau")]
        "luau" => self::luau::build(ctx),
        #[cfg(feature = "lib-url")]
        "url" => self::url::build(ctx),
        #[cfg(feature = "lib-semver")]
        "semver" => self::semver::build(ctx),
        #[cfg(feature = "lib-archive")]
        "archive" => self::archive::build(ctx),
        #[cfg(feature = "lib-sqlite")]
        "sqlite" => self::sqlite::build(ctx),
        #[cfg(feature = "lib-mongo")]
        "mongo" => self::mongo::build(ctx),
        #[cfg(feature = "lib-net")]
        "net" => self::net::build(ctx),
        #[cfg(feature = "lib-random")]
        "random" => self::random::build(ctx),
        #[cfg(feature = "lib-task")]
        "task" => self::task::build(ctx),
        #[cfg(feature = "lib-canvas")]
        "canvas" => self::canvas::build(ctx),
        #[cfg(feature = "lib-cache")]
        "cache" => self::cache::build(ctx),
        "dll" => self::foreign::build(ctx),
        other => Err(LehuaError::msg(format!(
            "built-in library '{other}' is not part of this runtime build"
        ))
        .into()),
    }
}

#[allow(dead_code)]
pub struct PathScope {
    base: PathBuf,
    engine: Arc<Engine>,
    lua: mlua::WeakLua,
}

#[allow(dead_code)]
impl PathScope {
    pub fn new(ctx: &LibCtx) -> Rc<Self> {
        Rc::new(PathScope {
            base: ctx.real_dir.clone(),
            engine: ctx.engine.clone(),
            lua: ctx.lua.weak(),
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
            } else if let Some(a) = self.engine.resolver.aliases.get(name) {
                self.engine.provider.base_dir().join(vpath::to_native(a))
            } else {
                return Err(LehuaError::msg(format!("unknown path alias '@{name}'")).into());
            };
            let joined = if sub.is_empty() { dir } else { dir.join(sub) };
            return Ok(normalize(&joined));
        }
        let pp = Path::new(p);
        if pp.is_absolute() {
            return Ok(normalize(pp));
        }
        let mut bases: Vec<PathBuf> = Vec::new();
        if let Some(lua) = self.lua.try_upgrade() {
            for id in self.engine.call_chain(&lua) {
                let dir = self.engine.real_dir_of(&id);
                if !bases.contains(&dir) {
                    bases.push(dir);
                }
            }
        }
        if !bases.contains(&self.base) {
            bases.push(self.base.clone());
        }
        for b in &bases {
            let cand = normalize(&b.join(pp));
            if cand.exists() {
                return Ok(cand);
            }
        }
        Ok(normalize(&bases[0].join(pp)))
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
