use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mlua::{AnyUserData, UserData, UserDataMethods, Value, Variadic};

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
            lua.create_async_function(move |lua, p: String| {
                let scope = scope.clone();
                async move {
                    let full = scope.resolve(&p)?;
                    let bytes = run_blocking(move || {
                        std::fs::read(&full).map_err(mlua::Error::external)
                    })
                    .await?;
                    lua.create_string(bytes)
                }
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "write",
            lua.create_async_function(move |_, (p, data): (String, mlua::LuaString)| {
                let scope = scope.clone();
                async move {
                    let full = scope.resolve(&p)?;
                    let bytes = data.as_bytes().to_vec();
                    run_blocking(move || {
                        std::fs::write(&full, &bytes).map_err(mlua::Error::external)?;
                        Ok(())
                    })
                    .await
                }
            })?,
        )?;
    }
    #[cfg(feature = "lib-canvas")]
    {
        let scope = scope.clone();
        t.set(
            "readImage",
            lua.create_async_function(move |_, p: String| {
                let scope = scope.clone();
                async move {
                    let full = scope.resolve(&p)?;
                    run_blocking(move || {
                        let bytes = std::fs::read(&full).map_err(mlua::Error::external)?;
                        super::canvas::decode_bytes(&bytes)
                    })
                    .await
                }
            })?,
        )?;
    }
    #[cfg(feature = "lib-canvas")]
    {
        let scope = scope.clone();
        t.set(
            "writeImage",
            lua.create_async_function(
                move |_, (p, image, format, quality): (String, mlua::AnyUserData, Option<String>, Option<u8>)| {
                    let scope = scope.clone();
                    async move {
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
                        let img = canvas.img.borrow().clone();
                        drop(canvas);
                        run_blocking(move || {
                            let bytes = super::canvas::encode_image(&img, fmt, quality)?;
                            if let Some(parent) = full.parent() {
                                std::fs::create_dir_all(parent).map_err(mlua::Error::external)?;
                            }
                            std::fs::write(&full, bytes).map_err(mlua::Error::external)?;
                            Ok(full.to_string_lossy().into_owned())
                        })
                        .await
                    }
                },
            )?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "readBuffer",
            lua.create_async_function(move |lua, p: String| {
                let scope = scope.clone();
                async move {
                    let full = scope.resolve(&p)?;
                    let bytes = run_blocking(move || {
                        std::fs::read(&full).map_err(mlua::Error::external)
                    })
                    .await?;
                    lua.create_buffer(bytes)
                }
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "writeBuffer",
            lua.create_async_function(move |_, (p, buf): (String, mlua::Buffer)| {
                let scope = scope.clone();
                async move {
                    let full = scope.resolve(&p)?;
                    let bytes = buf.to_vec();
                    run_blocking(move || {
                        std::fs::write(&full, bytes).map_err(mlua::Error::external)?;
                        Ok(())
                    })
                    .await
                }
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "readBufferAt",
            lua.create_async_function(move |lua, (p, start, end): (String, i64, i64)| {
                let scope = scope.clone();
                async move {
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
                    let bytes = run_blocking(move || {
                        let mut f = File::open(&full).map_err(mlua::Error::external)?;
                        f.seek(SeekFrom::Start(start as u64))
                            .map_err(mlua::Error::external)?;
                        let mut bytes = Vec::new();
                        Read::by_ref(&mut f)
                            .take((end - start) as u64)
                            .read_to_end(&mut bytes)
                            .map_err(mlua::Error::external)?;
                        Ok(bytes)
                    })
                    .await?;
                    lua.create_buffer(bytes)
                }
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "writeBufferAt",
            lua.create_async_function(move |_, (p, start, buf): (String, i64, mlua::Buffer)| {
                let scope = scope.clone();
                async move {
                    if start < 0 {
                        return Err(LehuaError::msg(
                            "fs.writeBufferAt: start must not be negative",
                        )
                        .into());
                    }
                    let full = scope.resolve(&p)?;
                    let bytes = buf.to_vec();
                    run_blocking(move || {
                        let mut f = OpenOptions::new()
                            .write(true)
                            .create(true)
                            .open(&full)
                            .map_err(mlua::Error::external)?;
                        f.seek(SeekFrom::Start(start as u64))
                            .map_err(mlua::Error::external)?;
                        f.write_all(&bytes).map_err(mlua::Error::external)?;
                        Ok(())
                    })
                    .await
                }
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "append",
            lua.create_async_function(move |_, (p, data): (String, mlua::LuaString)| {
                let scope = scope.clone();
                async move {
                    let full = scope.resolve(&p)?;
                    let bytes = data.as_bytes().to_vec();
                    run_blocking(move || {
                        let mut f = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&full)
                            .map_err(mlua::Error::external)?;
                        f.write_all(&bytes).map_err(mlua::Error::external)?;
                        Ok(())
                    })
                    .await
                }
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
            lua.create_async_function(move |_, p: String| {
                let scope = scope.clone();
                async move {
                    let full = scope.resolve(&p)?;
                    run_blocking(move || {
                        let r = if full.is_dir() {
                            std::fs::remove_dir_all(&full)
                        } else {
                            std::fs::remove_file(&full)
                        };
                        r.map_err(mlua::Error::external)?;
                        Ok(())
                    })
                    .await
                }
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "copy",
            lua.create_async_function(move |_, (src, dst): (String, String)| {
                let scope = scope.clone();
                async move {
                    let src = scope.resolve(&src)?;
                    let dst = scope.resolve(&dst)?;
                    run_blocking(move || {
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
                    })
                    .await
                }
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "copyAll",
            lua.create_async_function(move |_, (src, dst): (String, String)| {
                let scope = scope.clone();
                async move {
                    let src = scope.resolve(&src)?;
                    let dst = scope.resolve(&dst)?;
                    run_blocking(move || {
                        if src.is_dir() && dst.starts_with(&src) {
                            return Err(LehuaError::msg(
                                "fs.copyAll: destination is inside the source folder",
                            )
                            .into());
                        }
                        copy_recursive(&src, &dst).map_err(mlua::Error::external)?;
                        Ok(())
                    })
                    .await
                }
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "move",
            lua.create_async_function(move |_, (src, dst): (String, String)| {
                let scope = scope.clone();
                async move {
                    let src = scope.resolve(&src)?;
                    let dst = scope.resolve(&dst)?;
                    run_blocking(move || {
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
                    })
                    .await
                }
            })?,
        )?;
    }
    {
        let scope = scope.clone();
        t.set(
            "glob",
            lua.create_async_function(move |lua, pattern: String| {
                let scope = scope.clone();
                async move {
                    let full = scope.resolve(&pattern)?;
                    let pattern = full.to_string_lossy().replace('\\', "/");
                    let paths = run_blocking(move || {
                        let mut out: Vec<String> = Vec::new();
                        for entry in glob::glob(&pattern)
                            .map_err(|e| LehuaError::msg(format!("invalid glob pattern: {e}")))?
                        {
                            if let Ok(path) = entry {
                                out.push(path.to_string_lossy().into_owned());
                            }
                        }
                        Ok(out)
                    })
                    .await?;
                    let out = lua.create_table()?;
                    for (i, path) in (1usize..).zip(paths) {
                        out.raw_seti(i, path)?;
                    }
                    Ok(out)
                }
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
            lua.create_async_function(move |lua, p: String| {
                let scope = scope.clone();
                async move {
                    let full = scope.resolve(&p)?;
                    let names = run_blocking(move || {
                        let mut out: Vec<String> = Vec::new();
                        for entry in std::fs::read_dir(&full).map_err(mlua::Error::external)? {
                            let entry = entry.map_err(mlua::Error::external)?;
                            out.push(entry.file_name().to_string_lossy().into_owned());
                        }
                        Ok(out)
                    })
                    .await?;
                    let out = lua.create_table()?;
                    for (i, name) in (1usize..).zip(names) {
                        out.raw_seti(i, name)?;
                    }
                    Ok(out)
                }
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "open",
            lua.create_async_function(move |lua, args: (String, Option<String>)| {
                let scope = scope.clone();
                async move {
                    let (p, mode) = args;
                    let mode = mode.unwrap_or_else(|| "r".to_string());
                    let full = scope.resolve(&p)?;
                    let file = run_blocking(move || {
                        open_with_mode(&full, &mode).map_err(mlua::Error::external)
                    })
                    .await?;
                    lua.create_userdata(FileHandle::new(file))
                }
            })?,
        )?;
    }

    Ok(Value::Table(t))
}

async fn run_blocking<T: Send + 'static>(
    f: impl FnOnce() -> mlua::Result<T> + Send + 'static,
) -> mlua::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| mlua::Error::external(LehuaError::msg(format!("fs: join error: {e}"))))?
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

struct FileState {
    file: File,
    rbuf: Vec<u8>,
}

type SharedFile = Arc<Mutex<Option<FileState>>>;

struct FileHandle {
    state: SharedFile,
}

impl FileHandle {
    fn new(file: File) -> Self {
        FileHandle {
            state: Arc::new(Mutex::new(Some(FileState {
                file,
                rbuf: Vec::new(),
            }))),
        }
    }
}

async fn with_state<T: Send + 'static>(
    state: SharedFile,
    f: impl FnOnce(&mut FileState) -> mlua::Result<T> + Send + 'static,
) -> mlua::Result<T> {
    tokio::task::spawn_blocking(move || {
        let mut guard = state
            .lock()
            .map_err(|_| LehuaError::msg("fs: file lock poisoned"))?;
        let s = guard
            .as_mut()
            .ok_or_else(|| LehuaError::msg("attempt to use a closed file"))?;
        f(s)
    })
    .await
    .map_err(|e| mlua::Error::external(LehuaError::msg(format!("fs: join error: {e}"))))?
}

impl UserData for FileHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("read", |lua, this, fmt: Option<Value>| {
            let state = this.state.clone();
            let spec = parse_read_spec(&fmt);
            async move {
                let spec = spec?;
                let out = with_state(state, move |s| read_blocking(s, spec)).await?;
                match out {
                    ReadOut::Nil => Ok(Value::Nil),
                    ReadOut::Bytes(bytes) => Ok(Value::String(lua.create_string(bytes)?)),
                    ReadOut::Number(n) => Ok(Value::Number(n)),
                }
            }
        });

        methods.add_async_method("write", |_, this, args: Variadic<Value>| {
            let state = this.state.clone();
            let data = write_bytes(&args);
            async move {
                let data = data?;
                with_state(state, move |s| {
                    rewind_state(s)?;
                    s.file.write_all(&data).map_err(mlua::Error::external)?;
                    Ok(())
                })
                .await
            }
        });

        methods.add_async_method("seek", |_, this, args: (Option<String>, Option<i64>)| {
            let state = this.state.clone();
            async move {
                let (whence, offset) = args;
                let whence = whence.unwrap_or_else(|| "cur".to_string());
                let offset = offset.unwrap_or(0);
                let pos = with_state(state, move |s| {
                    rewind_state(s)?;
                    let from = match whence.as_str() {
                        "set" => SeekFrom::Start(offset.max(0) as u64),
                        "end" => SeekFrom::End(offset),
                        _ => SeekFrom::Current(offset),
                    };
                    s.file.seek(from).map_err(mlua::Error::external)
                })
                .await?;
                Ok(pos as i64)
            }
        });

        methods.add_function("lines", |lua, this_ud: AnyUserData| {
            let state = this_ud.borrow::<FileHandle>()?.state.clone();
            lua.create_function(move |lua, ()| {
                let mut guard = state
                    .lock()
                    .map_err(|_| LehuaError::msg("fs: file lock poisoned"))?;
                let s = guard
                    .as_mut()
                    .ok_or_else(|| LehuaError::msg("attempt to use a closed file"))?;
                match read_line(s)? {
                    Some(mut line) => {
                        if line.last() == Some(&b'\n') {
                            line.pop();
                            if line.last() == Some(&b'\r') {
                                line.pop();
                            }
                        }
                        Ok(Value::String(lua.create_string(&line)?))
                    }
                    None => Ok(Value::Nil),
                }
            })
        });

        methods.add_async_method("flush", |_, this, _: ()| {
            let state = this.state.clone();
            async move {
                with_state(state, |s| {
                    s.file.flush().map_err(mlua::Error::external)?;
                    Ok(())
                })
                .await
            }
        });

        methods.add_async_method("close", |_, this, _: ()| {
            let state = this.state.clone();
            async move {
                tokio::task::spawn_blocking(move || -> mlua::Result<()> {
                    let mut guard = state
                        .lock()
                        .map_err(|_| mlua::Error::from(LehuaError::msg("fs: file lock poisoned")))?;
                    let _ = guard.take();
                    Ok(())
                })
                .await
                .map_err(|e| {
                    mlua::Error::external(LehuaError::msg(format!("fs: join error: {e}")))
                })??;
                Ok(())
            }
        });
    }
}

fn write_bytes(args: &Variadic<Value>) -> mlua::Result<Vec<u8>> {
    let mut out = Vec::new();
    for a in args.iter() {
        match a {
            Value::String(s) => out.extend_from_slice(&s.as_bytes()[..]),
            Value::Integer(i) => out.extend_from_slice(i.to_string().as_bytes()),
            Value::Number(n) => out.extend_from_slice(n.to_string().as_bytes()),
            _ => {
                return Err(mlua::Error::external(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "file:write expects strings or numbers",
                )))
            }
        }
    }
    Ok(out)
}

fn rewind_state(s: &mut FileState) -> mlua::Result<()> {
    let n = s.rbuf.len();
    if n > 0 {
        s.file
            .seek(SeekFrom::Current(-(n as i64)))
            .map_err(mlua::Error::external)?;
        s.rbuf.clear();
    }
    Ok(())
}

enum ReadSpec {
    Count(usize),
    All,
    Line(bool),
    Number,
}

enum ReadOut {
    Nil,
    Bytes(Vec<u8>),
    Number(f64),
}

fn parse_read_spec(fmt: &Option<Value>) -> mlua::Result<ReadSpec> {
    if let Some(Value::Integer(n)) = fmt {
        return Ok(ReadSpec::Count((*n).max(0) as usize));
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
        "a" => Ok(ReadSpec::All),
        "l" => Ok(ReadSpec::Line(true)),
        "L" => Ok(ReadSpec::Line(false)),
        "n" => Ok(ReadSpec::Number),
        other => Err(mlua::Error::external(LehuaError::msg(format!(
            "invalid read format '{other}'"
        )))),
    }
}

fn read_blocking(s: &mut FileState, spec: ReadSpec) -> mlua::Result<ReadOut> {
    match spec {
        ReadSpec::Count(n) => {
            if n == 0 {
                if s.rbuf.is_empty() {
                    let mut probe = [0u8; 1];
                    let got = s.file.read(&mut probe).map_err(mlua::Error::external)?;
                    if got == 0 {
                        return Ok(ReadOut::Nil);
                    }
                    s.rbuf.push(probe[0]);
                }
                return Ok(ReadOut::Bytes(Vec::new()));
            }
            let bytes = read_n(s, n)?;
            if bytes.is_empty() {
                return Ok(ReadOut::Nil);
            }
            Ok(ReadOut::Bytes(bytes))
        }
        ReadSpec::All => {
            let mut data = s.rbuf.split_off(0);
            let mut rest = Vec::new();
            s.file.read_to_end(&mut rest).map_err(mlua::Error::external)?;
            data.extend_from_slice(&rest);
            Ok(ReadOut::Bytes(data))
        }
        ReadSpec::Line(strip) => match read_line(s)? {
            Some(mut line) => {
                if strip && line.last() == Some(&b'\n') {
                    line.pop();
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                }
                Ok(ReadOut::Bytes(line))
            }
            None => Ok(ReadOut::Nil),
        },
        ReadSpec::Number => match read_line(s)? {
            Some(line) => {
                let text = String::from_utf8_lossy(&line);
                match text.trim().parse::<f64>() {
                    Ok(num) => Ok(ReadOut::Number(num)),
                    Err(_) => Ok(ReadOut::Nil),
                }
            }
            None => Ok(ReadOut::Nil),
        },
    }
}

fn read_n(s: &mut FileState, n: usize) -> mlua::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(n.min(1 << 20));
    let take = n.min(s.rbuf.len());
    out.extend(s.rbuf.drain(..take));
    let mut chunk = [0u8; 64 * 1024];
    while out.len() < n {
        let want = (n - out.len()).min(chunk.len());
        let got = s
            .file
            .read(&mut chunk[..want])
            .map_err(mlua::Error::external)?;
        if got == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..got]);
    }
    Ok(out)
}

fn read_line(s: &mut FileState) -> mlua::Result<Option<Vec<u8>>> {
    loop {
        if let Some(pos) = s.rbuf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = s.rbuf.drain(..=pos).collect();
            return Ok(Some(line));
        }
        let mut chunk = [0u8; 4096];
        let got = s.file.read(&mut chunk).map_err(mlua::Error::external)?;
        if got == 0 {
            if s.rbuf.is_empty() {
                return Ok(None);
            }
            let line = s.rbuf.split_off(0);
            return Ok(Some(line));
        }
        s.rbuf.extend_from_slice(&chunk[..got]);
    }
}
