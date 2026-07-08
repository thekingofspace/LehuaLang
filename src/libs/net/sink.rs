use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::rc::Rc;

use mlua::{MetaMethod, UserData, UserDataMethods, Value};

use super::bytestream::value_bytes;
use crate::error::LehuaError;
use crate::libs::PathScope;

enum Store {
    Memory(Vec<u8>),
    File { path: PathBuf, handle: File },
}

pub struct Sink {
    inner: Rc<RefCell<Store>>,
    scope: Rc<PathScope>,
}

impl Sink {
    pub fn append(&self, data: &[u8]) -> mlua::Result<()> {
        let mut store = self.inner.borrow_mut();
        match &mut *store {
            Store::Memory(buf) => buf.extend_from_slice(data),
            Store::File { handle, .. } => {
                handle.write_all(data).map_err(mlua::Error::external)?;
            }
        }
        Ok(())
    }

    pub fn contents(&self) -> mlua::Result<Vec<u8>> {
        let mut store = self.inner.borrow_mut();
        match &mut *store {
            Store::Memory(buf) => Ok(buf.clone()),
            Store::File { handle, .. } => {
                handle.flush().map_err(mlua::Error::external)?;
                let mut f = handle.try_clone().map_err(mlua::Error::external)?;
                f.seek(SeekFrom::Start(0)).map_err(mlua::Error::external)?;
                let mut out = Vec::new();
                f.read_to_end(&mut out).map_err(mlua::Error::external)?;
                Ok(out)
            }
        }
    }

    fn len(&self) -> mlua::Result<u64> {
        let mut store = self.inner.borrow_mut();
        match &mut *store {
            Store::Memory(buf) => Ok(buf.len() as u64),
            Store::File { handle, .. } => {
                handle.flush().map_err(mlua::Error::external)?;
                Ok(handle.metadata().map_err(mlua::Error::external)?.len())
            }
        }
    }
}

pub fn new_memory(scope: Rc<PathScope>) -> Sink {
    Sink {
        inner: Rc::new(RefCell::new(Store::Memory(Vec::new()))),
        scope,
    }
}

pub fn new_file(scope: Rc<PathScope>, path: &str) -> mlua::Result<Sink> {
    let full = scope.resolve(path)?;
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).map_err(mlua::Error::external)?;
    }
    let handle = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(&full)
        .map_err(mlua::Error::external)?;
    Ok(Sink {
        inner: Rc::new(RefCell::new(Store::File { path: full, handle })),
        scope,
    })
}

impl UserData for Sink {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("write", |_, this, data: Value| {
            this.append(&value_bytes(&data)?)?;
            Ok(())
        });

        m.add_method("bytes", |lua, this, ()| {
            lua.create_string(this.contents()?)
        });

        m.add_method("buffer", |lua, this, ()| {
            lua.create_buffer(this.contents()?)
        });

        m.add_method("len", |_, this, ()| this.len());

        m.add_method("clear", |_, this, ()| {
            let mut store = this.inner.borrow_mut();
            match &mut *store {
                Store::Memory(buf) => buf.clear(),
                Store::File { handle, .. } => {
                    handle.set_len(0).map_err(mlua::Error::external)?;
                    handle
                        .seek(SeekFrom::Start(0))
                        .map_err(mlua::Error::external)?;
                }
            }
            Ok(())
        });

        m.add_method("save", |_, this, dest: String| {
            let target = this.scope.resolve(&dest)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(mlua::Error::external)?;
            }
            let data = this.contents()?;
            std::fs::write(&target, &data).map_err(mlua::Error::external)?;
            Ok(target.to_string_lossy().into_owned())
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

        m.add_meta_method(MetaMethod::Len, |_, this, ()| Ok(this.len()? as i64));

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

pub fn take_sink_bytes(v: &Value) -> Option<mlua::Result<Vec<u8>>> {
    v.as_userdata()
        .and_then(|u| u.borrow::<Sink>().ok())
        .map(|s| s.contents())
}

pub fn append_to_sink(v: &Value, data: &[u8]) -> Option<mlua::Result<()>> {
    v.as_userdata()
        .and_then(|u| u.borrow::<Sink>().ok())
        .map(|s| s.append(data))
}

pub fn bad_source() -> mlua::Error {
    LehuaError::msg("expected a string, buffer, or sink").into()
}
