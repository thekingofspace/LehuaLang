use mlua::{Lua, Table, UserData, UserDataMethods, Value};
use regex::bytes::{Regex, RegexBuilder};

use super::LibCtx;
use crate::error::LehuaError;

struct LuaRegex {
    re: Regex,
}

fn build_regex(pattern: &str, flags: &str) -> mlua::Result<Regex> {
    let mut b = RegexBuilder::new(pattern);
    for f in flags.chars() {
        match f {
            'i' => b.case_insensitive(true),
            'm' => b.multi_line(true),
            's' => b.dot_matches_new_line(true),
            'x' => b.ignore_whitespace(true),
            'U' => b.swap_greed(true),
            other => {
                return Err(LehuaError::msg(format!(
                    "unknown regex flag '{other}' (supported: i, m, s, x, U)"
                ))
                .into())
            }
        };
    }
    b.build()
        .map_err(|e| LehuaError::msg(format!("invalid regex: {e}")).into())
}

fn match_table(lua: &Lua, text: &[u8], start: usize, end: usize) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("text", lua.create_string(&text[start..end])?)?;
    t.set("start", start + 1)?;
    t.set("finish", end)?;
    Ok(t)
}

fn captures_table(
    lua: &Lua,
    re: &Regex,
    caps: &regex::bytes::Captures,
) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for name in re.capture_names().flatten() {
        if let Some(g) = caps.name(name) {
            let gt = lua.create_table()?;
            gt.set("text", lua.create_string(g.as_bytes())?)?;
            gt.set("start", g.start() + 1)?;
            gt.set("finish", g.end())?;
            t.set(name, gt)?;
        }
    }
    for i in 1..caps.len() {
        match caps.get(i) {
            Some(g) => {
                let gt = lua.create_table()?;
                gt.set("text", lua.create_string(g.as_bytes())?)?;
                gt.set("start", g.start() + 1)?;
                gt.set("finish", g.end())?;
                t.raw_seti(i, gt)?;
            }
            None => t.raw_seti(i, false)?,
        }
    }
    let whole = caps.get(0).unwrap();
    let wt = lua.create_table()?;
    wt.set("text", lua.create_string(whole.as_bytes())?)?;
    wt.set("start", whole.start() + 1)?;
    wt.set("finish", whole.end())?;
    t.set("match", wt)?;
    Ok(t)
}

impl UserData for LuaRegex {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("isMatch", |_, this, text: mlua::LuaString| {
            Ok(this.re.is_match(&text.as_bytes()))
        });

        m.add_method(
            "find",
            |lua, this, (text, init): (mlua::LuaString, Option<usize>)| {
                let bytes = text.as_bytes();
                let start = init.unwrap_or(1).max(1) - 1;
                if start > bytes.len() {
                    return Ok(Value::Nil);
                }
                match this.re.find_at(&bytes, start) {
                    Some(mt) => Ok(Value::Table(match_table(lua, &bytes, mt.start(), mt.end())?)),
                    None => Ok(Value::Nil),
                }
            },
        );

        m.add_method("findAll", |lua, this, text: mlua::LuaString| {
            let bytes = text.as_bytes();
            let out = lua.create_table()?;
            for (i, mt) in (1usize..).zip(this.re.find_iter(&bytes)) {
                out.raw_seti(i, match_table(lua, &bytes, mt.start(), mt.end())?)?;
            }
            Ok(out)
        });

        m.add_method("captures", |lua, this, text: mlua::LuaString| {
            let bytes = text.as_bytes();
            match this.re.captures(&bytes) {
                Some(caps) => Ok(Value::Table(captures_table(lua, &this.re, &caps)?)),
                None => Ok(Value::Nil),
            }
        });

        m.add_method("capturesAll", |lua, this, text: mlua::LuaString| {
            let bytes = text.as_bytes();
            let out = lua.create_table()?;
            for (i, caps) in (1usize..).zip(this.re.captures_iter(&bytes)) {
                out.raw_seti(i, captures_table(lua, &this.re, &caps)?)?;
            }
            Ok(out)
        });

        m.add_method(
            "replace",
            |lua, this, (text, replacement, limit): (mlua::LuaString, mlua::LuaString, Option<usize>)| {
                let limit = limit.unwrap_or(1);
                if limit == 0 {
                    return Ok(text);
                }
                let bytes = text.as_bytes();
                let rep = replacement.as_bytes();
                let out = this.re.replacen(&bytes, limit, &rep[..]);
                lua.create_string(out)
            },
        );

        m.add_method(
            "replaceAll",
            |lua, this, (text, replacement): (mlua::LuaString, mlua::LuaString)| {
                let bytes = text.as_bytes();
                let rep = replacement.as_bytes();
                let out = this.re.replace_all(&bytes, &rep[..]);
                lua.create_string(out)
            },
        );

        m.add_method(
            "split",
            |lua, this, (text, limit): (mlua::LuaString, Option<usize>)| {
                let bytes = text.as_bytes();
                let out = lua.create_table()?;
                match limit {
                    Some(n) => {
                        for (i, part) in (1usize..).zip(this.re.splitn(&bytes, n.max(1))) {
                            out.raw_seti(i, lua.create_string(part)?)?;
                        }
                    }
                    None => {
                        for (i, part) in (1usize..).zip(this.re.split(&bytes)) {
                            out.raw_seti(i, lua.create_string(part)?)?;
                        }
                    }
                }
                Ok(out)
            },
        );

        m.add_meta_method(mlua::MetaMethod::ToString, |_, this, ()| {
            Ok(format!("Regex({})", this.re.as_str()))
        });
    }
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    t.set(
        "new",
        lua.create_function(|_, (pattern, flags): (String, Option<String>)| {
            let re = build_regex(&pattern, flags.as_deref().unwrap_or(""))?;
            Ok(LuaRegex { re })
        })?,
    )?;

    t.set(
        "escape",
        lua.create_function(|_, text: String| Ok(regex::escape(&text)))?,
    )?;

    t.set(
        "isMatch",
        lua.create_function(|_, (pattern, text): (String, mlua::LuaString)| {
            let re = build_regex(&pattern, "")?;
            Ok(re.is_match(&text.as_bytes()))
        })?,
    )?;

    Ok(Value::Table(t))
}
