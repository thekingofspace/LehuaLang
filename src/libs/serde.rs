use mlua::serde::SerializeOptions;
use mlua::{Lua, LuaSerdeExt, Table, Value};

use super::LibCtx;
use crate::error::LehuaError;

const FORMATS: &[&str] = &["json", "yaml", "toml"];

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    let formats = lua.create_table()?;
    for (i, f) in (1usize..).zip(FORMATS.iter()) {
        formats.raw_seti(i, *f)?;
    }
    t.set("formats", formats)?;

    t.set("null", lua.null())?;

    t.set(
        "array",
        lua.create_function(|lua, t: Option<Table>| {
            let t = match t {
                Some(t) => t,
                None => lua.create_table()?,
            };
            t.set_metatable(Some(lua.array_metatable()))?;
            Ok(t)
        })?,
    )?;

    t.set(
        "encode",
        lua.create_function(|_, (format, data, pretty): (String, Value, Option<bool>)| {
            check_depth(&data)?;
            encode(&format, &data, pretty.unwrap_or(false))
        })?,
    )?;

    t.set(
        "decode",
        lua.create_function(|lua, (format, text): (String, String)| decode(lua, &format, &text))?,
    )?;

    Ok(Value::Table(t))
}

fn check_depth(data: &Value) -> mlua::Result<()> {
    const MAX: usize = 128;
    let mut visited = std::collections::HashSet::new();
    let mut stack: Vec<(Table, usize)> = Vec::new();
    if let Value::Table(t) = data {
        visited.insert(t.to_pointer() as usize);
        stack.push((t.clone(), 1));
    }
    while let Some((t, depth)) = stack.pop() {
        if depth > MAX {
            return Err(LehuaError::msg(format!(
                "serde.encode: data is nested more than {MAX} levels deep"
            ))
            .into());
        }
        for pair in t.pairs::<Value, Value>() {
            let (k, v) = pair?;
            for child in [k, v] {
                if let Value::Table(ct) = child {
                    if visited.insert(ct.to_pointer() as usize) {
                        stack.push((ct, depth + 1));
                    }
                }
            }
        }
    }
    Ok(())
}

fn bad_format(format: &str) -> mlua::Error {
    LehuaError::msg(format!(
        "unknown serde format '{format}' (supported: {})",
        FORMATS.join(", ")
    ))
    .into()
}

fn norm(format: &str) -> String {
    let f = format.trim().to_ascii_lowercase();
    if f == "yml" {
        "yaml".to_string()
    } else {
        f
    }
}

fn encode(format: &str, data: &Value, pretty: bool) -> mlua::Result<String> {
    match norm(format).as_str() {
        "json" => {
            if pretty {
                serde_json::to_string_pretty(data).map_err(mlua::Error::external)
            } else {
                serde_json::to_string(data).map_err(mlua::Error::external)
            }
        }
        "yaml" => serde_yaml_ng::to_string(data).map_err(mlua::Error::external),
        "toml" => {
            if pretty {
                toml::to_string_pretty(data).map_err(mlua::Error::external)
            } else {
                toml::to_string(data).map_err(mlua::Error::external)
            }
        }
        _ => Err(bad_format(format)),
    }
}

fn decode(lua: &Lua, format: &str, text: &str) -> mlua::Result<Value> {
    let opts = SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false);
    match norm(format).as_str() {
        "json" => {
            let v: serde_json::Value = serde_json::from_str(text).map_err(mlua::Error::external)?;
            lua.to_value_with(&v, opts)
        }
        "yaml" => {
            let v: serde_yaml_ng::Value =
                serde_yaml_ng::from_str(text).map_err(mlua::Error::external)?;
            lua.to_value_with(&v, opts)
        }
        "toml" => {
            let v: toml::Value = toml::from_str(text).map_err(mlua::Error::external)?;
            toml_to_lua(lua, v)
        }
        _ => Err(bad_format(format)),
    }
}

fn toml_to_lua(lua: &Lua, v: toml::Value) -> mlua::Result<Value> {
    Ok(match v {
        toml::Value::String(s) => Value::String(lua.create_string(&s)?),
        toml::Value::Integer(i) => Value::Integer(i),
        toml::Value::Float(f) => Value::Number(f),
        toml::Value::Boolean(b) => Value::Boolean(b),
        toml::Value::Datetime(d) => Value::String(lua.create_string(d.to_string())?),
        toml::Value::Array(items) => {
            let t = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in (1usize..).zip(items) {
                t.raw_seti(i, toml_to_lua(lua, item)?)?;
            }
            t.set_metatable(Some(lua.array_metatable()))?;
            Value::Table(t)
        }
        toml::Value::Table(map) => {
            let t = lua.create_table_with_capacity(0, map.len())?;
            for (k, item) in map {
                t.raw_set(k, toml_to_lua(lua, item)?)?;
            }
            Value::Table(t)
        }
    })
}
