use mlua::{Lua, Table, Value};
use percent_encoding::{percent_decode, percent_encode, AsciiSet, NON_ALPHANUMERIC};

use super::LibCtx;
use crate::error::LehuaError;

const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

fn parse_url(lua: &Lua, text: &str) -> mlua::Result<Table> {
    let u = ::url::Url::parse(text)
        .map_err(|e| LehuaError::msg(format!("url.parse: {e}")))?;
    let t = lua.create_table()?;
    t.set("href", u.as_str())?;
    t.set("scheme", u.scheme())?;
    if let Some(host) = u.host_str() {
        t.set("host", host)?;
    }
    if let Some(port) = u.port() {
        t.set("port", port)?;
    }
    t.set("path", u.path())?;
    if !u.username().is_empty() {
        t.set("username", u.username())?;
    }
    if let Some(p) = u.password() {
        t.set("password", p)?;
    }
    if let Some(q) = u.query() {
        t.set("query", q)?;
        t.set("queryParams", decode_query_bytes(lua, q.as_bytes())?)?;
    }
    if let Some(f) = u.fragment() {
        t.set("fragment", f)?;
    }
    Ok(t)
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    t.set(
        "parse",
        lua.create_function(|lua, text: String| parse_url(lua, &text))?,
    )?;

    t.set(
        "join",
        lua.create_function(|_, (base, reference): (String, String)| {
            let joined = ::url::Url::parse(&base)
                .and_then(|b| b.join(&reference))
                .map_err(|e| LehuaError::msg(format!("url.join: {e}")))?;
            Ok(joined.to_string())
        })?,
    )?;

    t.set(
        "isValid",
        lua.create_function(|_, text: String| Ok(::url::Url::parse(&text).is_ok()))?,
    )?;

    t.set(
        "encode",
        lua.create_function(|_, text: mlua::LuaString| {
            Ok(percent_encode(&text.as_bytes(), COMPONENT).to_string())
        })?,
    )?;

    t.set(
        "decode",
        lua.create_function(|lua, text: mlua::LuaString| {
            let bytes: Vec<u8> = percent_decode(&text.as_bytes()).collect();
            lua.create_string(bytes)
        })?,
    )?;

    t.set(
        "encodeQuery",
        lua.create_function(|_, params: Table| {
            let mut ser = ::url::form_urlencoded::Serializer::new(String::new());
            let mut pairs: Vec<(String, String)> = Vec::new();
            for entry in params.pairs::<String, Value>() {
                let (k, v) = entry?;
                let v = match v {
                    Value::String(s) => s.to_str()?.to_string(),
                    Value::Integer(i) => i.to_string(),
                    Value::Number(n) => n.to_string(),
                    Value::Boolean(b) => b.to_string(),
                    other => {
                        return Err(LehuaError::msg(format!(
                            "url.encodeQuery: value for '{k}' must be a string, number, or boolean, got {}",
                            other.type_name()
                        ))
                        .into())
                    }
                };
                pairs.push((k, v));
            }
            pairs.sort();
            for (k, v) in pairs {
                ser.append_pair(&k, &v);
            }
            Ok(ser.finish())
        })?,
    )?;

    t.set(
        "decodeQuery",
        lua.create_function(|lua, text: mlua::LuaString| {
            let bytes = text.as_bytes();
            let trimmed = bytes.strip_prefix(b"?").unwrap_or(&bytes);
            decode_query_bytes(lua, trimmed)
        })?,
    )?;

    Ok(Value::Table(t))
}

fn decode_query_bytes(lua: &Lua, query: &[u8]) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for pair in query.split(|&b| b == b'&') {
        if pair.is_empty() {
            continue;
        }
        let eq = pair.iter().position(|&b| b == b'=');
        let (k, v) = match eq {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => (pair, &pair[..0]),
        };
        let decode = |part: &[u8]| -> Vec<u8> {
            let plus_to_space: Vec<u8> = part
                .iter()
                .map(|&b| if b == b'+' { b' ' } else { b })
                .collect();
            percent_decode(&plus_to_space).collect()
        };
        t.set(lua.create_string(decode(k))?, lua.create_string(decode(v))?)?;
    }
    Ok(t)
}
