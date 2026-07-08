use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use mlua::{AnyUserData, Lua, UserData, UserDataMethods, Value, Variadic};

use super::{normalize, LibCtx, PathScope};
use crate::error::LehuaError;

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;
    let scope = PathScope::new(ctx);

    t.set("dir", scope.base().to_string_lossy().into_owned())?;

    {
        let scope = scope.clone();
        t.set(
            "resolve",
            lua.create_function(move |_, p: String| {
                Ok(scope.resolve(&p)?.to_string_lossy().into_owned())
            })?,
        )?;
    }

    t.set(
        "join",
        lua.create_function(|_, parts: Variadic<String>| {
            let mut pb = PathBuf::new();
            for p in parts.iter() {
                pb.push(p);
            }
            Ok(normalize(&pb).to_string_lossy().into_owned())
        })?,
    )?;

    t.set(
        "dirname",
        lua.create_function(|_, p: String| {
            Ok(Path::new(&p)
                .parent()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_default())
        })?,
    )?;

    t.set(
        "basename",
        lua.create_function(|_, p: String| {
            Ok(Path::new(&p)
                .file_name()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_default())
        })?,
    )?;

    t.set(
        "extname",
        lua.create_function(|_, p: String| {
            Ok(Path::new(&p)
                .extension()
                .map(|x| format!(".{}", x.to_string_lossy()))
                .unwrap_or_default())
        })?,
    )?;

    t.set(
        "cwd",
        lua.create_function(|_, ()| {
            Ok(std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default())
        })?,
    )?;

    macro_rules! predicate {
        ($name:literal, $f:expr) => {{
            let scope = scope.clone();
            t.set(
                $name,
                lua.create_function(move |_, p: String| {
                    let full = scope.resolve(&p)?;
                    let f: fn(&Path) -> bool = $f;
                    Ok(f(&full))
                })?,
            )?;
        }};
    }
    predicate!("exists", |p| p.exists());
    predicate!("isFile", |p| p.is_file());
    predicate!("isDir", |p| p.is_dir());

    {
        let scope = scope.clone();
        t.set(
            "metadata",
            lua.create_function(move |lua, p: String| {
                let full = scope.resolve(&p)?;
                let meta = std::fs::metadata(&full).map_err(mlua::Error::external)?;
                let out = lua.create_table()?;
                let kind = if meta.is_dir() {
                    "dir"
                } else if meta.is_file() {
                    "file"
                } else {
                    "other"
                };
                out.set("type", kind)?;
                out.set("size", meta.len() as f64)?;
                out.set("readonly", meta.permissions().readonly())?;
                if let Ok(time) = meta.modified() {
                    out.set("modifiedAt", unix_secs(time))?;
                }
                if let Ok(time) = meta.created() {
                    out.set("createdAt", unix_secs(time))?;
                }
                if let Ok(time) = meta.accessed() {
                    out.set("accessedAt", unix_secs(time))?;
                }
                Ok(out)
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "read",
            lua.create_function(move |lua, p: String| {
                let full = scope.resolve(&p)?;
                let bytes = std::fs::read(&full).map_err(mlua::Error::external)?;
                lua.create_string(bytes)
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "write",
            lua.create_function(move |_, (p, data): (String, mlua::LuaString)| {
                let full = scope.resolve(&p)?;
                std::fs::write(&full, &data.as_bytes()[..]).map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "append",
            lua.create_function(move |_, (p, data): (String, mlua::LuaString)| {
                let full = scope.resolve(&p)?;
                let mut f = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&full)
                    .map_err(mlua::Error::external)?;
                f.write_all(&data.as_bytes()[..]).map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "mkdir",
            lua.create_function(move |_, p: String| {
                std::fs::create_dir_all(scope.resolve(&p)?).map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "remove",
            lua.create_function(move |_, p: String| {
                let full = scope.resolve(&p)?;
                let r = if full.is_dir() {
                    std::fs::remove_dir(&full)
                } else {
                    std::fs::remove_file(&full)
                };
                r.map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "removeAll",
            lua.create_function(move |_, p: String| {
                let full = scope.resolve(&p)?;
                let r = if full.is_dir() {
                    std::fs::remove_dir_all(&full)
                } else {
                    std::fs::remove_file(&full)
                };
                r.map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "readDir",
            lua.create_function(move |lua, p: String| {
                let full = scope.resolve(&p)?;
                let out = lua.create_table()?;
                for (i, entry) in (1usize..).zip(std::fs::read_dir(&full).map_err(mlua::Error::external)?) {
                    let entry = entry.map_err(mlua::Error::external)?;
                    out.raw_seti(i, entry.file_name().to_string_lossy().into_owned())?;
                }
                Ok(out)
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "open",
            lua.create_function(move |lua, args: (String, Option<String>)| {
                let (p, mode) = args;
                let mode = mode.unwrap_or_else(|| "r".to_string());
                let full = scope.resolve(&p)?;
                let file = open_with_mode(&full, &mode).map_err(mlua::Error::external)?;
                lua.create_userdata(FileHandle::new(file))
            })?,
        )?;
    }

    Ok(Value::Table(t))
}

fn unix_secs(t: std::time::SystemTime) -> f64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn open_with_mode(path: &Path, mode: &str) -> std::io::Result<File> {
    let mut opts = OpenOptions::new();
    let m = mode.replace('b', "");
    match m.as_str() {
        "r" => {
            opts.read(true);
        }
        "r+" => {
            opts.read(true).write(true);
        }
        "w" => {
            opts.write(true).create(true).truncate(true);
        }
        "w+" => {
            opts.read(true).write(true).create(true).truncate(true);
        }
        "a" => {
            opts.append(true).create(true);
        }
        "a+" => {
            opts.read(true).append(true).create(true);
        }
        _ => {
            opts.read(true);
        }
    }
    opts.open(path)
}

struct FileHandle {
    file: RefCell<Option<File>>,
    rbuf: RefCell<Vec<u8>>,
}

impl FileHandle {
    fn new(file: File) -> Self {
        FileHandle {
            file: RefCell::new(Some(file)),
            rbuf: RefCell::new(Vec::new()),
        }
    }

    fn with_file<T>(&self, f: impl FnOnce(&mut File) -> std::io::Result<T>) -> mlua::Result<T> {
        let mut guard = self.file.borrow_mut();
        let file = guard
            .as_mut()
            .ok_or_else(|| LehuaError::msg("attempt to use a closed file"))?;
        f(file).map_err(mlua::Error::external)
    }
}

impl UserData for FileHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("read", |lua, this, fmt: Option<Value>| read_impl(lua, this, fmt));

        methods.add_method("write", |_, this, args: Variadic<Value>| {
            rewind_buffer(this)?;
            this.with_file(|f| {
                for a in args.iter() {
                    match a {
                        Value::String(s) => f.write_all(&s.as_bytes()[..])?,
                        Value::Integer(i) => f.write_all(i.to_string().as_bytes())?,
                        Value::Number(n) => f.write_all(n.to_string().as_bytes())?,
                        _ => {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "file:write expects strings or numbers",
                            ))
                        }
                    }
                }
                Ok(())
            })?;
            Ok(())
        });

        methods.add_method("seek", |_, this, args: (Option<String>, Option<i64>)| {
            let (whence, offset) = args;
            let whence = whence.unwrap_or_else(|| "cur".to_string());
            let offset = offset.unwrap_or(0);
            rewind_buffer(this)?;
            let pos = this.with_file(|f| {
                let from = match whence.as_str() {
                    "set" => SeekFrom::Start(offset.max(0) as u64),
                    "end" => SeekFrom::End(offset),
                    _ => SeekFrom::Current(offset),
                };
                f.seek(from)
            })?;
            Ok(pos as i64)
        });

        methods.add_function("lines", |lua, this_ud: AnyUserData| {
            lua.create_function(move |lua, ()| {
                let handle = this_ud.borrow::<FileHandle>()?;
                read_impl(lua, &handle, Some(Value::String(lua.create_string("l")?)))
            })
        });

        methods.add_method("flush", |_, this, _: ()| {
            this.with_file(|f| f.flush())?;
            Ok(())
        });

        methods.add_method("close", |_, this, _: ()| {
            this.rbuf.borrow_mut().clear();
            *this.file.borrow_mut() = None;
            Ok(())
        });
    }
}

fn rewind_buffer(this: &FileHandle) -> mlua::Result<()> {
    let n = this.rbuf.borrow().len();
    if n > 0 {
        this.with_file(|f| f.seek(SeekFrom::Current(-(n as i64))))?;
        this.rbuf.borrow_mut().clear();
    }
    Ok(())
}

fn read_impl(lua: &Lua, this: &FileHandle, fmt: Option<Value>) -> mlua::Result<Value> {
    if let Some(Value::Integer(n)) = &fmt {
        let n = (*n).max(0) as usize;
        let bytes = read_n(this, n)?;
        if bytes.is_empty() && n > 0 {
            return Ok(Value::Nil);
        }
        return Ok(Value::String(lua.create_string(bytes)?));
    }

    let spec = match fmt {
        Some(Value::String(s)) => s.to_string_lossy().to_string(),
        None => "l".to_string(),
        Some(other) => {
            return Err(mlua::Error::external(LehuaError::msg(format!(
                "invalid read format: {other:?}"
            ))))
        }
    };
    let spec = spec.trim_start_matches('*');

    match spec {
        "a" => {
            let mut data = this.rbuf.borrow_mut().split_off(0);
            let mut rest = Vec::new();
            this.with_file(|f| f.read_to_end(&mut rest))?;
            data.extend_from_slice(&rest);
            Ok(Value::String(lua.create_string(data)?))
        }
        "l" | "L" => match read_line(this)? {
            Some(mut line) => {
                if spec == "l" && line.last() == Some(&b'\n') {
                    line.pop();
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                }
                Ok(Value::String(lua.create_string(line)?))
            }
            None => Ok(Value::Nil),
        },
        "n" => match read_line(this)? {
            Some(line) => {
                let s = String::from_utf8_lossy(&line);
                match s.trim().parse::<f64>() {
                    Ok(num) => Ok(Value::Number(num)),
                    Err(_) => Ok(Value::Nil),
                }
            }
            None => Ok(Value::Nil),
        },
        other => Err(mlua::Error::external(LehuaError::msg(format!(
            "invalid read format '{other}'"
        )))),
    }
}

fn read_n(this: &FileHandle, n: usize) -> mlua::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(n);
    {
        let mut buf = this.rbuf.borrow_mut();
        let take = n.min(buf.len());
        out.extend(buf.drain(..take));
    }
    if out.len() < n {
        let mut remaining = vec![0u8; n - out.len()];
        let got = this.with_file(|f| f.read(&mut remaining))?;
        out.extend_from_slice(&remaining[..got]);
    }
    Ok(out)
}

fn read_line(this: &FileHandle) -> mlua::Result<Option<Vec<u8>>> {
    loop {
        let newline = this.rbuf.borrow().iter().position(|&b| b == b'\n');
        if let Some(pos) = newline {
            let line: Vec<u8> = this.rbuf.borrow_mut().drain(..=pos).collect();
            return Ok(Some(line));
        }
        let mut chunk = [0u8; 4096];
        let got = this.with_file(|f| f.read(&mut chunk))?;
        if got == 0 {
            let mut buf = this.rbuf.borrow_mut();
            if buf.is_empty() {
                return Ok(None);
            }
            let line = buf.split_off(0);
            return Ok(Some(line));
        }
        this.rbuf.borrow_mut().extend_from_slice(&chunk[..got]);
    }
}
