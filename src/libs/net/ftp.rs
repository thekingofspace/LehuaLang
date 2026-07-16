use std::io::Cursor;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use mlua::{Lua, MetaMethod, Table, UserData, UserDataMethods, Value};
use suppaftp::types::FileType;
use suppaftp::FtpStream;

use super::sink::{append_to_sink, bad_source, take_sink_bytes};
use crate::error::LehuaError;
use crate::libs::PathScope;

type Shared = Arc<Mutex<Option<FtpStream>>>;

async fn with_ftp<T, F>(ftp: Shared, f: F) -> mlua::Result<T>
where
    F: FnOnce(&mut FtpStream) -> suppaftp::FtpResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || -> mlua::Result<T> {
        let mut guard = ftp
            .lock()
            .map_err(|_| LehuaError::msg("ftp: session lock was poisoned"))?;
        let stream = guard
            .as_mut()
            .ok_or_else(|| LehuaError::msg("this FTP session is closed"))?;
        f(stream).map_err(|e| mlua::Error::external(LehuaError::msg(format!("ftp: {e}"))))
    })
    .await
    .map_err(|e| mlua::Error::external(LehuaError::msg(format!("ftp task failed: {e}"))))?
}

async fn source_bytes(v: &Value) -> mlua::Result<Vec<u8>> {
    match v {
        Value::String(s) => Ok(s.as_bytes().to_vec()),
        Value::Buffer(b) => Ok(b.to_vec()),
        Value::UserData(_) => match take_sink_bytes(v).await {
            Some(bytes) => bytes,
            None => Err(bad_source()),
        },
        _ => Err(bad_source()),
    }
}

pub struct FtpSession {
    ftp: Shared,
    scope: Rc<PathScope>,
    label: String,
}

impl UserData for FtpSession {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_async_method("pwd", |_, this, ()| {
            let ftp = this.ftp.clone();
            async move { with_ftp(ftp, |s| s.pwd()).await }
        });

        m.add_async_method("cwd", |_, this, path: String| {
            let ftp = this.ftp.clone();
            async move { with_ftp(ftp, move |s| s.cwd(&path)).await }
        });

        m.add_async_method("cdup", |_, this, ()| {
            let ftp = this.ftp.clone();
            async move { with_ftp(ftp, |s| s.cdup()).await }
        });

        m.add_async_method("mkdir", |_, this, path: String| {
            let ftp = this.ftp.clone();
            async move { with_ftp(ftp, move |s| s.mkdir(&path)).await }
        });

        m.add_async_method("removeDir", |_, this, path: String| {
            let ftp = this.ftp.clone();
            async move { with_ftp(ftp, move |s| s.rmdir(&path)).await }
        });

        m.add_async_method("remove", |_, this, path: String| {
            let ftp = this.ftp.clone();
            async move { with_ftp(ftp, move |s| s.rm(&path)).await }
        });

        m.add_async_method("rename", |_, this, (from, to): (String, String)| {
            let ftp = this.ftp.clone();
            async move { with_ftp(ftp, move |s| s.rename(&from, &to)).await }
        });

        m.add_async_method("size", |_, this, path: String| {
            let ftp = this.ftp.clone();
            async move {
                let n = with_ftp(ftp, move |s| s.size(&path)).await?;
                Ok(n as f64)
            }
        });

        m.add_async_method("list", |lua, this, path: Option<String>| {
            let ftp = this.ftp.clone();
            async move {
                let entries = with_ftp(ftp, move |s| s.list(path.as_deref())).await?;
                let out = lua.create_table()?;
                for (i, e) in (1usize..).zip(entries) {
                    out.raw_seti(i, e)?;
                }
                Ok(out)
            }
        });

        m.add_async_method("nameList", |lua, this, path: Option<String>| {
            let ftp = this.ftp.clone();
            async move {
                let entries = with_ftp(ftp, move |s| s.nlst(path.as_deref())).await?;
                let out = lua.create_table()?;
                for (i, e) in (1usize..).zip(entries) {
                    out.raw_seti(i, e)?;
                }
                Ok(out)
            }
        });

        m.add_async_method("binary", |_, this, ()| {
            let ftp = this.ftp.clone();
            async move { with_ftp(ftp, |s| s.transfer_type(FileType::Binary)).await }
        });

        m.add_async_method("ascii", |_, this, ()| {
            let ftp = this.ftp.clone();
            async move {
                with_ftp(ftp, |s| {
                    s.transfer_type(FileType::Ascii(suppaftp::types::FormatControl::Default))
                })
                .await
            }
        });

        m.add_async_method("download", |lua, this, (remote, into): (String, Option<Value>)| {
            let ftp = this.ftp.clone();
            async move {
                let bytes = with_ftp(ftp, move |s| {
                    s.transfer_type(FileType::Binary)?;
                    Ok(s.retr_as_buffer(&remote)?.into_inner())
                })
                .await?;
                match into {
                    Some(v) => match append_to_sink(&v, &bytes).await {
                        Some(res) => {
                            res?;
                            Ok(Value::Integer(bytes.len() as i64))
                        }
                        None => Err(LehuaError::msg(
                            "download: the second argument must be a sink from net.sink()",
                        )
                        .into()),
                    },
                    None => Ok(Value::String(lua.create_string(&bytes)?)),
                }
            }
        });

        m.add_async_method("downloadTo", |_, this, (remote, local): (String, String)| {
            let ftp = this.ftp.clone();
            let target = this.scope.resolve(&local);
            async move {
                let target = target?;
                let bytes = with_ftp(ftp, move |s| {
                    s.transfer_type(FileType::Binary)?;
                    Ok(s.retr_as_buffer(&remote)?.into_inner())
                })
                .await?;
                tokio::task::spawn_blocking(move || {
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent).map_err(mlua::Error::external)?;
                    }
                    std::fs::write(&target, &bytes).map_err(mlua::Error::external)?;
                    Ok(target.to_string_lossy().into_owned())
                })
                .await
                .map_err(|e| {
                    mlua::Error::external(LehuaError::msg(format!("ftp task failed: {e}")))
                })?
            }
        });

        m.add_async_method("upload", |_, this, (remote, source): (String, Value)| {
            let ftp = this.ftp.clone();
            async move {
                let data = source_bytes(&source).await?;
                let n = with_ftp(ftp, move |s| {
                    s.transfer_type(FileType::Binary)?;
                    let mut cursor = Cursor::new(data);
                    s.put_file(&remote, &mut cursor)
                })
                .await?;
                Ok(n as f64)
            }
        });

        m.add_async_method("uploadFile", |_, this, (remote, local): (String, String)| {
            let ftp = this.ftp.clone();
            let source = this.scope.resolve(&local);
            async move {
                let source = source?;
                let data = tokio::task::spawn_blocking(move || {
                    std::fs::read(&source).map_err(mlua::Error::external)
                })
                .await
                .map_err(|e| {
                    mlua::Error::external(LehuaError::msg(format!("ftp task failed: {e}")))
                })??;
                let n = with_ftp(ftp, move |s| {
                    s.transfer_type(FileType::Binary)?;
                    let mut cursor = Cursor::new(data);
                    s.put_file(&remote, &mut cursor)
                })
                .await?;
                Ok(n as f64)
            }
        });

        m.add_async_method("noop", |_, this, ()| {
            let ftp = this.ftp.clone();
            async move { with_ftp(ftp, |s| s.noop()).await }
        });

        m.add_async_method("close", |_, this, ()| {
            let ftp = this.ftp.clone();
            async move {
                let _ = tokio::task::spawn_blocking(move || {
                    if let Ok(mut guard) = ftp.lock() {
                        if let Some(mut stream) = guard.take() {
                            let _ = stream.quit();
                        }
                    }
                })
                .await;
                Ok(())
            }
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("FtpSession({})", this.label))
        });
    }
}

pub fn install(lua: &Lua, net: &Table, scope: Rc<PathScope>) -> mlua::Result<()> {
    let ftp = lua.create_table()?;

    ftp.set(
        "connect",
        lua.create_async_function(move |lua, opts: Table| {
            let scope = scope.clone();
            async move {
                let host = opts
                    .get::<Option<String>>("host")?
                    .ok_or_else(|| LehuaError::msg("ftp.connect: 'host' is required"))?;
                let port = opts.get::<Option<u16>>("port")?.unwrap_or(21);
                let user = opts.get::<Option<String>>("user")?;
                let password = opts.get::<Option<String>>("password")?;
                let addr = format!("{host}:{port}");
                let label = addr.clone();
                let stream = tokio::task::spawn_blocking(move || -> mlua::Result<FtpStream> {
                    let mut s = FtpStream::connect(&addr).map_err(|e| {
                        mlua::Error::external(LehuaError::msg(format!("ftp connect: {e}")))
                    })?;
                    if let Some(u) = user {
                        s.login(u.as_str(), password.as_deref().unwrap_or(""))
                            .map_err(|e| {
                                mlua::Error::external(LehuaError::msg(format!("ftp login: {e}")))
                            })?;
                    }
                    Ok(s)
                })
                .await
                .map_err(|e| {
                    mlua::Error::external(LehuaError::msg(format!("ftp task failed: {e}")))
                })??;
                let session = FtpSession {
                    ftp: Arc::new(Mutex::new(Some(stream))),
                    scope,
                    label,
                };
                Ok(Value::UserData(lua.create_userdata(session)?))
            }
        })?,
    )?;

    net.set("ftp", ftp)?;
    Ok(())
}
