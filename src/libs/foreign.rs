use mlua::Value;

use super::LibCtx;
use crate::dll;
use crate::error::LehuaError;

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    let provider = ctx.engine.provider.clone();
    let cache = ctx.dlls.clone();
    let from_id = ctx.from_id.clone();

    let open = lua.create_function(move |lua, entry: String| {
        if entry.trim().is_empty() {
            return Err(LehuaError::msg("dll.open: a library path is required").into());
        }
        let id = dll::dll_id(&from_id, &entry);
        dll::open_table(lua, provider.clone(), cache.clone(), id)
    })?;

    t.set("open", open.clone())?;
    t.set("load", open)?;

    Ok(Value::Table(t))
}
