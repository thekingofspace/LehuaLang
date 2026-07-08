use mlua::{Lua, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::error::LehuaError;

pub async fn read_exact<R: AsyncReadExt + Unpin>(
    reader: &mut BufReader<R>,
    n: usize,
) -> mlua::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(n.min(1 << 20));
    let mut chunk = [0u8; 64 * 1024];
    while out.len() < n {
        let want = (n - out.len()).min(chunk.len());
        let got = reader
            .read(&mut chunk[..want])
            .await
            .map_err(mlua::Error::external)?;
        if got == 0 {
            break;
        }
        out.extend_from_slice(&chunk[..got]);
    }
    Ok(out)
}

pub async fn read_some<R: AsyncReadExt + Unpin>(
    reader: &mut BufReader<R>,
    max: usize,
) -> mlua::Result<Vec<u8>> {
    let cap = max.min(64 * 1024);
    let mut buf = vec![0u8; cap];
    let got = reader.read(&mut buf).await.map_err(mlua::Error::external)?;
    buf.truncate(got);
    Ok(buf)
}

pub async fn read_all<R: AsyncReadExt + Unpin>(
    reader: &mut BufReader<R>,
) -> mlua::Result<Vec<u8>> {
    let mut out = Vec::new();
    reader
        .read_to_end(&mut out)
        .await
        .map_err(mlua::Error::external)?;
    Ok(out)
}

pub async fn read_line<R: AsyncReadExt + Unpin>(
    reader: &mut BufReader<R>,
) -> mlua::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let got = reader
        .read_until(b'\n', &mut line)
        .await
        .map_err(mlua::Error::external)?;
    if got == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    Ok(Some(line))
}

pub async fn read_until<R: AsyncReadExt + Unpin>(
    reader: &mut BufReader<R>,
    delim: u8,
) -> mlua::Result<Option<Vec<u8>>> {
    let mut buf = Vec::new();
    let got = reader
        .read_until(delim, &mut buf)
        .await
        .map_err(mlua::Error::external)?;
    if got == 0 {
        return Ok(None);
    }
    if buf.last() == Some(&delim) {
        buf.pop();
    }
    Ok(Some(buf))
}

pub async fn write_all<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    data: &[u8],
) -> mlua::Result<()> {
    writer.write_all(data).await.map_err(mlua::Error::external)?;
    writer.flush().await.map_err(mlua::Error::external)?;
    Ok(())
}

pub fn value_bytes(v: &Value) -> mlua::Result<Vec<u8>> {
    match v {
        Value::String(s) => Ok(s.as_bytes().to_vec()),
        Value::Buffer(b) => Ok(b.to_vec()),
        Value::Integer(i) => Ok(i.to_string().into_bytes()),
        Value::Number(n) => Ok(n.to_string().into_bytes()),
        other => Err(LehuaError::msg(format!(
            "expected a string or buffer to send, got {}",
            other.type_name()
        ))
        .into()),
    }
}

pub fn opt_bytes_to_lua(lua: &Lua, bytes: Option<Vec<u8>>) -> mlua::Result<Value> {
    match bytes {
        Some(b) => Ok(Value::String(lua.create_string(b)?)),
        None => Ok(Value::Nil),
    }
}
