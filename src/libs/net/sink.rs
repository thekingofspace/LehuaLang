use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use mlua::{MetaMethod, UserData, UserDataMethods, Value};

use super::bytestream::value_bytes;
use crate::error::LehuaError;
use crate::libs::PathScope;

type SharedFile = Arc<Mutex<File>>;

enum Store {
    Memory(Vec<u8>),
    File { path: PathBuf, handle: SharedFile },
}

enum Snapshot {
    Memory(Vec<u8>),
    File(SharedFile),
}

pub struct Sink {
    inner: Rc<RefCell<Store>>,
    scope: Rc<PathScope>,
}

async fn run_file<T: Send + 'static>(
    handle: SharedFile,
    f: impl FnOnce(&mut File) -> mlua::Result<T> + Send + 'static,
) -> mlua::Result<T> {
    tokio::task::spawn_blocking(move || {
        let mut file = handle
            .lock()
            .map_err(|_| LehuaError::msg("sink: file lock poisoned"))?;
        f(&mut file)
    })
    .await
    .map_err(|e| mlua::Error::external(LehuaError::msg(format!("sink: join error: {e}"))))?
}

fn read_back(handle: &mut File) -> mlua::Result<Vec<u8>> {
    handle.flush().map_err(mlua::Error::external)?;
    let mut f = handle.try_clone().map_err(mlua::Error::external)?;
    f.seek(SeekFrom::Start(0)).map_err(mlua::Error::external)?;
    let mut out = Vec::new();
    f.read_to_end(&mut out).map_err(mlua::Error::external)?;
    Ok(out)
}

fn file_len(handle: &mut File) -> mlua::Result<u64> {
    handle.flush().map_err(mlua::Error::external)?;
    Ok(handle.metadata().map_err(mlua::Error::external)?.len())
}

fn ensure_parent(target: &Path) -> mlua::Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(mlua::Error::external)?;
    }
    Ok(())
}

impl Sink {
    fn snapshot(&self) -> Snapshot {
        match &*self.inner.borrow() {
            Store::Memory(buf) => Snapshot::Memory(buf.clone()),
            Store::File { handle, .. } => Snapshot::File(handle.clone()),
        }
    }

    pub async fn append(&self, data: &[u8]) -> mlua::Result<()> {
        let handle = {
            let mut store = self.inner.borrow_mut();
            match &mut *store {
                Store::Memory(buf) => {
                    buf.extend_from_slice(data);
                    return Ok(());
                }
                Store::File { handle, .. } => handle.clone(),
            }
        };
        let data = data.to_vec();
        run_file(handle, move |f| {
            f.write_all(&data).map_err(mlua::Error::external)
        })
        .await
    }

    pub async fn contents(&self) -> mlua::Result<Vec<u8>> {
        match self.snapshot() {
            Snapshot::Memory(buf) => Ok(buf),
            Snapshot::File(handle) => run_file(handle, read_back).await,
        }
    }

    async fn len(&self) -> mlua::Result<u64> {
        let handle = {
            let store = self.inner.borrow();
            match &*store {
                Store::Memory(buf) => return Ok(buf.len() as u64),
                Store::File { handle, .. } => handle.clone(),
            }
        };
        run_file(handle, file_len).await
    }
}

pub fn new_memory(scope: Rc<PathScope>) -> Sink {
    Sink {
        inner: Rc::new(RefCell::new(Store::Memory(Vec::new()))),
        scope,
    }
}

pub async fn new_file(scope: Rc<PathScope>, path: &str) -> mlua::Result<Sink> {
    let full = scope.resolve(path)?;
    let target = full.clone();
    let handle = tokio::task::spawn_blocking(move || {
        ensure_parent(&target)?;
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&target)
            .map_err(mlua::Error::external)
    })
    .await
    .map_err(|e| mlua::Error::external(LehuaError::msg(format!("sink: join error: {e}"))))??;
    Ok(Sink {
        inner: Rc::new(RefCell::new(Store::File {
            path: full,
            handle: Arc::new(Mutex::new(handle)),
        })),
        scope,
    })
}

impl UserData for Sink {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_async_method("write", |_, this, data: Value| {
            let bytes = value_bytes(&data);
            async move {
                let bytes = bytes?;
                this.append(&bytes).await
            }
        });

        m.add_async_method("bytes", |lua, this, ()| async move {
            lua.create_string(this.contents().await?)
        });

        m.add_async_method("buffer", |lua, this, ()| async move {
            lua.create_buffer(this.contents().await?)
        });

        m.add_async_method("len", |_, this, ()| async move { this.len().await });

        m.add_async_method("clear", |_, this, ()| async move {
            let handle = {
                let mut store = this.inner.borrow_mut();
                match &mut *store {
                    Store::Memory(buf) => {
                        buf.clear();
                        return Ok(());
                    }
                    Store::File { handle, .. } => handle.clone(),
                }
            };
            run_file(handle, |f| {
                f.set_len(0).map_err(mlua::Error::external)?;
                f.seek(SeekFrom::Start(0)).map_err(mlua::Error::external)?;
                Ok(())
            })
            .await
        });

        m.add_async_method("save", |_, this, dest: String| {
            let target = this.scope.resolve(&dest);
            let snap = this.snapshot();
            async move {
                let target = target?;
                match snap {
                    Snapshot::Memory(data) => tokio::task::spawn_blocking(move || {
                        ensure_parent(&target)?;
                        std::fs::write(&target, &data).map_err(mlua::Error::external)?;
                        Ok(target.to_string_lossy().into_owned())
                    })
                    .await
                    .map_err(|e| {
                        mlua::Error::external(LehuaError::msg(format!("sink: join error: {e}")))
                    })?,
                    Snapshot::File(handle) => {
                        run_file(handle, move |f| {
                            ensure_parent(&target)?;
                            let data = read_back(f)?;
                            std::fs::write(&target, &data).map_err(mlua::Error::external)?;
                            Ok(target.to_string_lossy().into_owned())
                        })
                        .await
                    }
                }
            }
        });

        m.add_method("isFile", |_, this, ()| {
            Ok(matches!(&*this.inner.borrow(), Store::File { .. }))
        });

        m.add_method("path", |_, this, ()| {
            Ok(match &*this.inner.borrow() {
                Store::File { path, .. } => Some(path.to_string_lossy().into_owned()),
                Store::Memory(_) => None,
            })
        });

        m.add_meta_method(MetaMethod::Len, |_, this, ()| {
            let len = match &*this.inner.borrow() {
                Store::Memory(buf) => buf.len() as u64,
                Store::File { handle, .. } => match handle.try_lock() {
                    Ok(mut file) => file_len(&mut file)?,
                    Err(std::sync::TryLockError::WouldBlock) => {
                        return Err(LehuaError::msg(
                            "sink: a file operation is in progress, use sink:len() instead of #sink",
                        )
                        .into())
                    }
                    Err(_) => return Err(LehuaError::msg("sink: file lock poisoned").into()),
                },
            };
            Ok(len as i64)
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(match &*this.inner.borrow() {
                Store::File { path, .. } => {
                    format!("Sink(file: {})", path.display())
                }
                Store::Memory(buf) => format!("Sink(memory: {} bytes)", buf.len()),
            })
        });
    }
}

pub async fn take_sink_bytes(v: &Value) -> Option<mlua::Result<Vec<u8>>> {
    let sink = v.as_userdata().and_then(|u| u.borrow::<Sink>().ok())?;
    Some(sink.contents().await)
}

pub async fn append_to_sink(v: &Value, data: &[u8]) -> Option<mlua::Result<()>> {
    let sink = v.as_userdata().and_then(|u| u.borrow::<Sink>().ok())?;
    Some(sink.append(data).await)
}

pub fn bad_source() -> mlua::Error {
    LehuaError::msg("expected a string, buffer, or sink").into()
}
