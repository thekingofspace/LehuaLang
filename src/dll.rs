use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use libloading::{Library, Symbol};
use mlua::{Function, Lua, Table, Value, Variadic};

use crate::engine::VmContext;
use crate::error::LehuaError;
use crate::portable::PortableValue;
use crate::provider::ModuleProvider;
use crate::vpath;

type LehuaFn = unsafe extern "C" fn(*const u8, usize, *mut usize) -> *mut u8;
type LehuaFree = unsafe extern "C" fn(*mut u8, usize);

type DllCache = Rc<RefCell<HashMap<String, Arc<Library>>>>;

pub fn dll_id(module_id: &str, entry: &str) -> String {
    vpath::join(&vpath::dirname(module_id), entry)
}

pub fn make_dll_global(
    lua: &Lua,
    ctx: &Rc<VmContext>,
    module_id: &str,
    entry: &str,
) -> mlua::Result<Value> {
    let id = dll_id(module_id, entry);
    open_table(lua, ctx.engine.provider.clone(), ctx.dlls.clone(), id)
}

pub fn open_table(
    lua: &Lua,
    provider: Arc<dyn ModuleProvider>,
    cache: DllCache,
    id: String,
) -> mlua::Result<Value> {
    let tbl = lua.create_table()?;
    let meta = lua.create_table()?;
    let index_fn = lua.create_function(move |lua, (t, name): (Table, String)| {
        let f = make_caller(lua, provider.clone(), cache.clone(), id.clone(), name.clone())?;
        t.raw_set(name, f.clone())?;
        Ok(f)
    })?;
    meta.set("__index", index_fn)?;
    tbl.set_metatable(Some(meta))?;
    Ok(Value::Table(tbl))
}

fn make_caller(
    lua: &Lua,
    provider: Arc<dyn ModuleProvider>,
    cache: DllCache,
    dll_id: String,
    name: String,
) -> mlua::Result<Function> {
    lua.create_async_function(move |lua, args: Variadic<Value>| {
        let provider = provider.clone();
        let cache = cache.clone();
        let dll_id = dll_id.clone();
        let name = name.clone();
        async move {
            let lib = load_cached(&provider, &cache, &dll_id)?;

            let json_args: Vec<serde_json::Value> = args
                .iter()
                .map(|v| PortableValue::from_lua(v).map(|p| p.to_json()))
                .collect::<crate::error::Result<_>>()?;
            let input = serde_json::to_vec(&serde_json::Value::Array(json_args))
                .map_err(mlua::Error::external)?;

            let call_name = name.clone();
            let call_id = dll_id.clone();
            let err_id = dll_id.clone();
            let resp = tokio::task::spawn_blocking(move || call_export(&lib, &call_id, &call_name, &input))
                .await
                .map_err(|e| {
                    mlua::Error::external(LehuaError::Dll {
                        lib: err_id,
                        message: format!("native call failed to join: {e}"),
                    })
                })??;

            response_to_lua(&lua, &dll_id, &resp)
        }
    })
}

fn load_cached(
    provider: &Arc<dyn ModuleProvider>,
    cache: &DllCache,
    id: &str,
) -> mlua::Result<Arc<Library>> {
    if let Some(l) = cache.borrow().get(id) {
        return Ok(l.clone());
    }
    let path = provider.binary_path(id)?;
    let lib = unsafe { Library::new(&path) }.map_err(|e| LehuaError::Dll {
        lib: id.to_string(),
        message: format!("failed to load '{}': {e}", path.display()),
    })?;
    let lib = Arc::new(lib);
    cache.borrow_mut().insert(id.to_string(), lib.clone());
    Ok(lib)
}

fn call_export(
    lib: &Library,
    dll_id: &str,
    name: &str,
    input: &[u8],
) -> mlua::Result<serde_json::Value> {
    unsafe {
        let func: Symbol<LehuaFn> = lib.get(name.as_bytes()).map_err(|e| {
            mlua::Error::external(LehuaError::Dll {
                lib: dll_id.to_string(),
                message: format!("export '{name}' not found: {e}"),
            })
        })?;
        let free: Symbol<LehuaFree> = lib.get(b"lehua_free").map_err(|e| {
            mlua::Error::external(LehuaError::Dll {
                lib: dll_id.to_string(),
                message: format!("DLL is missing the required 'lehua_free' export: {e}"),
            })
        })?;

        let mut out_len: usize = 0;
        let out_ptr = func(input.as_ptr(), input.len(), &mut out_len as *mut usize);
        if out_ptr.is_null() {
            return Ok(serde_json::json!({ "ok": serde_json::Value::Null }));
        }
        let bytes = std::slice::from_raw_parts(out_ptr, out_len).to_vec();
        free(out_ptr, out_len);
        serde_json::from_slice(&bytes).map_err(mlua::Error::external)
    }
}

fn response_to_lua(lua: &Lua, dll_id: &str, resp: &serde_json::Value) -> mlua::Result<Value> {
    if let Some(err) = resp.get("err") {
        let msg = err
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| err.to_string());
        return Err(mlua::Error::external(LehuaError::Dll {
            lib: dll_id.to_string(),
            message: msg,
        }));
    }
    let ok = resp.get("ok").cloned().unwrap_or(serde_json::Value::Null);
    PortableValue::from_json(&ok).into_lua(lua)
}
