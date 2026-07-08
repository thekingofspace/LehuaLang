use mlua::{Lua, Table, Value};

use super::LibCtx;
use crate::error::LehuaError;

fn parse_version(text: &str) -> mlua::Result<::semver::Version> {
    ::semver::Version::parse(text.trim())
        .map_err(|e| LehuaError::msg(format!("invalid version '{text}': {e}")).into())
}

fn version_table(lua: &Lua, v: &::semver::Version) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("major", v.major)?;
    t.set("minor", v.minor)?;
    t.set("patch", v.patch)?;
    if !v.pre.is_empty() {
        t.set("prerelease", v.pre.as_str())?;
    }
    if !v.build.is_empty() {
        t.set("build", v.build.as_str())?;
    }
    Ok(t)
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    t.set(
        "parse",
        lua.create_function(|lua, text: String| {
            let v = parse_version(&text)?;
            version_table(lua, &v)
        })?,
    )?;

    t.set(
        "isValid",
        lua.create_function(|_, text: String| {
            Ok(::semver::Version::parse(text.trim()).is_ok())
        })?,
    )?;

    t.set(
        "compare",
        lua.create_function(|_, (a, b): (String, String)| {
            let a = parse_version(&a)?;
            let b = parse_version(&b)?;
            Ok(match a.cmp_precedence(&b) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            })
        })?,
    )?;

    t.set(
        "satisfies",
        lua.create_function(|_, (version, requirement): (String, String)| {
            let v = parse_version(&version)?;
            let req = ::semver::VersionReq::parse(requirement.trim()).map_err(|e| {
                LehuaError::msg(format!("invalid requirement '{requirement}': {e}"))
            })?;
            Ok(req.matches(&v))
        })?,
    )?;

    t.set(
        "sort",
        lua.create_function(|lua, versions: Vec<String>| {
            let mut parsed: Vec<::semver::Version> = Vec::with_capacity(versions.len());
            for v in &versions {
                parsed.push(parse_version(v)?);
            }
            parsed.sort_by(|a, b| a.cmp_precedence(b));
            let out = lua.create_table_with_capacity(parsed.len(), 0)?;
            for (i, v) in (1usize..).zip(parsed.iter()) {
                out.raw_seti(i, v.to_string())?;
            }
            Ok(out)
        })?,
    )?;

    Ok(Value::Table(t))
}
