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
    #[cfg(feature = "lib-canvas")]
    {
        let scope = scope.clone();
        t.set(
            "readImage",
            lua.create_function(move |_, p: String| {
                let full = scope.resolve(&p)?;
                let bytes = std::fs::read(&full).map_err(mlua::Error::external)?;
                super::canvas::decode_bytes(&bytes)
            })?,
        )?;
    }
    #[cfg(feature = "lib-canvas")]
    {
        let scope = scope.clone();
        t.set(
            "writeImage",
            lua.create_function(
                move |_, (p, image, format, quality): (String, mlua::AnyUserData, Option<String>, Option<u8>)| {
                    let canvas = image.borrow::<super::canvas::Canvas>().map_err(|_| {
                        crate::error::LehuaError::msg("writeImage expects a canvas as its second argument")
                    })?;
                    let full = scope.resolve(&p)?;
                    let name = match format {
                        Some(f) => f,
                        None => full
                            .extension()
                            .map(|e| e.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "png".to_string()),
                    };
                    let fmt = super::canvas::format_from_name(&name)?;
                    let bytes = super::canvas::encode_image(&canvas.img.borrow(), fmt, quality)?;
                    if let Some(parent) = full.parent() {
                        std::fs::create_dir_all(parent).map_err(mlua::Error::external)?;
                    }
                    std::fs::write(&full, bytes).map_err(mlua::Error::external)?;
                    Ok(full.to_string_lossy().into_owned())
                },
            )?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "readBuffer",
            lua.create_function(move |lua, p: String| {
                let full = scope.resolve(&p)?;
                let bytes = std::fs::read(&full).map_err(mlua::Error::external)?;
                lua.create_buffer(bytes)
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "writeBuffer",
            lua.create_function(move |_, (p, buf): (String, mlua::Buffer)| {
                let full = scope.resolve(&p)?;
                std::fs::write(&full, buf.to_vec()).map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "readBufferAt",
            lua.create_function(move |lua, (p, start, end): (String, i64, i64)| {
                if start < 0 || end < 0 {
                    return Err(LehuaError::msg(
                        "fs.readBufferAt: start and end must not be negative",
                    )
                    .into());
                }
                if end < start {
                    return Err(LehuaError::msg(
                        "fs.readBufferAt: end must not be before start",
                    )
                    .into());
                }
                let full = scope.resolve(&p)?;
                let mut f = File::open(&full).map_err(mlua::Error::external)?;
                f.seek(SeekFrom::Start(start as u64))
                    .map_err(mlua::Error::external)?;
                let mut bytes = Vec::new();
                Read::by_ref(&mut f)
                    .take((end - start) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(mlua::Error::external)?;
                lua.create_buffer(bytes)
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "writeBufferAt",
            lua.create_function(move |_, (p, start, buf): (String, i64, mlua::Buffer)| {
                if start < 0 {
                    return Err(LehuaError::msg(
                        "fs.writeBufferAt: start must not be negative",
                    )
                    .into());
                }
                let full = scope.resolve(&p)?;
                let mut f = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .open(&full)
                    .map_err(mlua::Error::external)?;
                f.seek(SeekFrom::Start(start as u64))
                    .map_err(mlua::Error::external)?;
                f.write_all(&buf.to_vec()).map_err(mlua::Error::external)?;
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
            "copy",
            lua.create_function(move |_, (src, dst): (String, String)| {
                let src = scope.resolve(&src)?;
                let dst = scope.resolve(&dst)?;
                if src.is_dir() {
                    return Err(LehuaError::msg(
                        "fs.copy copies files; use fs.copyAll for folders",
                    )
                    .into());
                }
                if src == dst {
                    return Err(LehuaError::msg(
                        "fs.copy: source and destination are the same file",
                    )
                    .into());
                }
                std::fs::copy(&src, &dst).map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "copyAll",
            lua.create_function(move |_, (src, dst): (String, String)| {
                let src = scope.resolve(&src)?;
                let dst = scope.resolve(&dst)?;
                if src.is_dir() && dst.starts_with(&src) {
                    return Err(LehuaError::msg(
                        "fs.copyAll: destination is inside the source folder",
                    )
                    .into());
                }
                copy_recursive(&src, &dst).map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "move",
            lua.create_function(move |_, (src, dst): (String, String)| {
                let src = scope.resolve(&src)?;
                let dst = scope.resolve(&dst)?;
                if src.is_dir() && dst.starts_with(&src) {
                    return Err(LehuaError::msg(
                        "fs.move: destination is inside the source folder",
                    )
                    .into());
                }
                if std::fs::rename(&src, &dst).is_ok() {
                    return Ok(());
                }
                copy_recursive(&src, &dst).map_err(mlua::Error::external)?;
                let r = if src.is_dir() {
                    std::fs::remove_dir_all(&src)
                } else {
                    std::fs::remove_file(&src)
                };
                r.map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "glob",
            lua.create_function(move |lua, pattern: String| {
                let full = scope.resolve(&pattern)?;
                let pattern = full.to_string_lossy().replace('\\', "/");
                let out = lua.create_table()?;
                let mut i = 1usize;
                for entry in glob::glob(&pattern)
                    .map_err(|e| LehuaError::msg(format!("invalid glob pattern: {e}")))?
                {
                    if let Ok(path) = entry {
                        out.raw_seti(i, path.to_string_lossy().into_owned())?;
                        i += 1;
                    }
                }
                Ok(out)
            })?,
        )?;
    }

    t.set(
        "tempDir",
        lua.create_function(|_, ()| {
            let path = std::env::temp_dir().join(format!("lehua-{}", random_suffix()?));
            std::fs::create_dir_all(&path).map_err(mlua::Error::external)?;
            Ok(path.to_string_lossy().into_owned())
        })?,
    )?;

    t.set(
        "tempFile",
        lua.create_function(|_, ext: Option<String>| {
            let ext = ext
                .map(|e| {
                    let e = e.trim_start_matches('.').to_string();
                    if e.is_empty() {
                        String::from("tmp")
                    } else {
                        e
                    }
                })
                .unwrap_or_else(|| String::from("tmp"));
            let path = std::env::temp_dir().join(format!("lehua-{}.{ext}", random_suffix()?));
            File::create(&path).map_err(mlua::Error::external)?;
            Ok(path.to_string_lossy().into_owned())
        })?,
    )?;

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

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

fn random_suffix() -> mlua::Result<String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes)
        .map_err(|e| LehuaError::msg(format!("random source failed: {e}")))?;
    let mut out = String::with_capacity(16);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    Ok(out)
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
        if n == 0 {
            if this.rbuf.borrow().is_empty() {
                let mut probe = [0u8; 1];
                let got = this.with_file(|f| f.read(&mut probe))?;
                if got == 0 {
                    return Ok(Value::Nil);
                }
                this.rbuf.borrow_mut().push(probe[0]);
            }
            return Ok(Value::String(lua.create_string("")?));
        }
        let bytes = read_n(this, n)?;
        if bytes.is_empty() {
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
    let mut out = Vec::with_capacity(n.min(1 << 20));
    {
        let mut buf = this.rbuf.borrow_mut();
        let take = n.min(buf.len());
        out.extend(buf.drain(..take));
    }
    let mut chunk = [0u8; 64 * 1024];
    while out.len() < n {
        let want = (n - out.len()).min(chunk.len());
        let got = this.with_file(|f| f.read(&mut chunk[..want]))?;
        if got == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..got]);
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
