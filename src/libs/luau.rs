use mlua::chunk::Compiler;
use mlua::{Function, Lua, MultiValue, Table, Value};

use super::LibCtx;

fn load_chunk(lua: &Lua, source: &[u8], opts: &Option<Table>) -> mlua::Result<Function> {
    let mut chunk = lua.load(source);
    if let Some(o) = opts {
        if let Some(name) = o.get::<Option<String>>("name")? {
            chunk = chunk.set_name(name);
        } else {
            chunk = chunk.set_name("luau.load");
        }
        if let Some(env) = o.get::<Option<Table>>("environment")? {
            chunk = chunk.set_environment(env);
        }
    } else {
        chunk = chunk.set_name("luau.load");
    }
    chunk.into_function()
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    t.set(
        "load",
        lua.create_function(|lua, (source, opts): (mlua::LuaString, Option<Table>)| {
            load_chunk(lua, &source.as_bytes(), &opts)
        })?,
    )?;

    t.set(
        "eval",
        lua.create_async_function(
            |lua, (source, opts): (mlua::LuaString, Option<Table>)| async move {
                let bytes = source.as_bytes();
                let mut as_expression = Vec::with_capacity(bytes.len() + 7);
                as_expression.extend_from_slice(b"return ");
                as_expression.extend_from_slice(&bytes);
                let func = match load_chunk(&lua, &as_expression, &opts) {
                    Ok(f) => f,
                    Err(_) => load_chunk(&lua, &bytes, &opts)?,
                };
                func.call_async::<MultiValue>(()).await
            },
        )?,
    )?;

    t.set(
        "compile",
        lua.create_function(|lua, (source, opts): (mlua::LuaString, Option<Table>)| {
            let mut compiler = Compiler::new();
            if let Some(o) = &opts {
                if let Some(level) = o.get::<Option<u8>>("optimizationLevel")? {
                    compiler = compiler.set_optimization_level(level);
                }
                if let Some(level) = o.get::<Option<u8>>("debugLevel")? {
                    compiler = compiler.set_debug_level(level);
                }
                if let Some(level) = o.get::<Option<u8>>("coverageLevel")? {
                    compiler = compiler.set_coverage_level(level);
                }
            }
            let bytecode = compiler.compile(&source.as_bytes())?;
            lua.create_string(bytecode)
        })?,
    )?;

    Ok(Value::Table(t))
}
