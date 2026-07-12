use std::collections::HashMap;

use mlua::{Function, Lua, MultiValue, Table, Value};

use crate::error::LehuaError;

fn lua_err(msg: impl Into<String>) -> mlua::Error {
    LehuaError::msg(msg.into()).into()
}

fn is_class_table(t: &Table) -> bool {
    match t.metatable() {
        Some(mt) => matches!(mt.raw_get::<Value>("__classdata"), Ok(Value::Boolean(true))),
        None => false,
    }
}

fn class_meta(class: &Table) -> mlua::Result<Table> {
    match class.metatable() {
        Some(mt) if matches!(mt.raw_get::<Value>("__classdata"), Ok(Value::Boolean(true))) => Ok(mt),
        _ => Err(lua_err("expected a ClassData value")),
    }
}

fn interface_meta(iface: &Value) -> mlua::Result<Table> {
    if let Value::Table(t) = iface {
        if let Some(mt) = t.metatable() {
            if matches!(mt.raw_get::<Value>("__interface"), Ok(Value::Boolean(true))) {
                return Ok(mt);
            }
        }
    }
    Err(lua_err("expected an Interface value"))
}

fn key_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.to_string_lossy(),
        other => value_display(other),
    }
}

fn value_display(v: &Value) -> String {
    match v {
        Value::Nil => "nil".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.to_string_lossy(),
        Value::Table(_) => "table".to_string(),
        Value::Function(_) => "function".to_string(),
        other => other.type_name().to_string(),
    }
}

fn resolve_static(meta: &Table, key: &Value) -> mlua::Result<Value> {
    let own_static: Table = meta.raw_get("ownStatic")?;
    let own: Value = own_static.raw_get(key.clone())?;
    if !own.is_nil() {
        return Ok(own);
    }
    let owners: Table = meta.raw_get("staticOwners")?;
    if let Value::Table(owner_class) = owners.raw_get::<Value>(key.clone())? {
        let owner_meta = class_meta(&owner_class)?;
        return resolve_static(&owner_meta, key);
    }
    Ok(Value::Nil)
}

fn static_exists(meta: &Table, key: &Value) -> mlua::Result<bool> {
    let own_static: Table = meta.raw_get("ownStatic")?;
    if !own_static.raw_get::<Value>(key.clone())?.is_nil() {
        return Ok(true);
    }
    let owners: Table = meta.raw_get("staticOwners")?;
    Ok(!owners.raw_get::<Value>(key.clone())?.is_nil())
}

fn static_definer(meta: &Table, key: &Value) -> mlua::Result<Value> {
    let own_static: Table = meta.raw_get("ownStatic")?;
    if !own_static.raw_get::<Value>(key.clone())?.is_nil() {
        return meta.raw_get("classRef");
    }
    let owners: Table = meta.raw_get("staticOwners")?;
    if let Value::Table(owner_class) = owners.raw_get::<Value>(key.clone())? {
        let owner_meta = class_meta(&owner_class)?;
        return static_definer(&owner_meta, key);
    }
    Ok(Value::Nil)
}

fn surface_has(meta: &Table, key: &Value) -> mlua::Result<bool> {
    let public: Table = meta.raw_get("public")?;
    if !public.raw_get::<Value>(key.clone())?.is_nil() {
        return Ok(true);
    }
    static_exists(meta, key)
}

fn check_interface(iface: &Value, meta: &Table) -> mlua::Result<()> {
    let imeta = interface_meta(iface)?;
    let requires: Table = imeta.raw_get("requires")?;
    let name: Value = imeta.raw_get("name")?;
    for entry in requires.sequence_values::<Value>() {
        let key = entry?;
        if !surface_has(meta, &key)? {
            return Err(lua_err(format!(
                "class does not implement interface '{}': missing '{}'",
                value_display(&name),
                key_str(&key)
            )));
        }
    }
    Ok(())
}

fn deep_copy(lua: &Lua, v: &Value, seen: &mut HashMap<usize, Table>) -> mlua::Result<Value> {
    let t = match v {
        Value::Table(t) => t,
        other => return Ok(other.clone()),
    };
    let ptr = t.to_pointer() as usize;
    if let Some(existing) = seen.get(&ptr) {
        return Ok(Value::Table(existing.clone()));
    }
    let out = lua.create_table()?;
    seen.insert(ptr, out.clone());
    for pair in t.pairs::<Value, Value>() {
        let (k, val) = pair?;
        let ck = deep_copy(lua, &k, seen)?;
        let cv = deep_copy(lua, &val, seen)?;
        out.raw_set(ck, cv)?;
    }
    if let Some(mt) = t.metatable() {
        out.set_metatable(Some(mt))?;
    }
    Ok(Value::Table(out))
}

fn translate(registry: &Table, v: Value) -> mlua::Result<Value> {
    if let Value::Table(t) = &v {
        if let Value::Table(internal) = registry.raw_get::<Value>(t.clone())? {
            return Ok(Value::Table(internal));
        }
    }
    Ok(v)
}

struct Entry {
    class: Table,
    all: bool,
    public: Option<Table>,
    private: Option<Table>,
    statics: Option<Table>,
    metamethods: Option<Table>,
}

fn opt_table(v: Value) -> Option<Table> {
    match v {
        Value::Table(t) => Some(t),
        _ => None,
    }
}

fn inherit_entries(spec: &Table) -> mlua::Result<Vec<Entry>> {
    match spec.raw_get::<Value>("Inherits")? {
        Value::Nil => Ok(Vec::new()),
        Value::Table(t) => {
            if is_class_table(&t) {
                return Ok(vec![Entry {
                    class: t,
                    all: true,
                    public: None,
                    private: None,
                    statics: None,
                    metamethods: None,
                }]);
            }
            let mut entries = Vec::new();
            for item in t.sequence_values::<Value>() {
                match item? {
                    Value::Table(e) if is_class_table(&e) => entries.push(Entry {
                        class: e,
                        all: true,
                        public: None,
                        private: None,
                        statics: None,
                        metamethods: None,
                    }),
                    Value::Table(e) => {
                        let class = match e.raw_get::<Value>("Class")? {
                            Value::Table(c) if is_class_table(&c) => c,
                            _ => {
                                return Err(lua_err(
                                    "each Inherits entry must be a ClassData or a table with a Class field",
                                ))
                            }
                        };
                        entries.push(Entry {
                            class,
                            all: false,
                            public: opt_table(e.raw_get("Public")?),
                            private: opt_table(e.raw_get("Private")?),
                            statics: opt_table(e.raw_get("Static")?),
                            metamethods: opt_table(e.raw_get("Metamethods")?),
                        });
                    }
                    _ => {
                        return Err(lua_err(
                            "each Inherits entry must be a ClassData or a table with a Class field",
                        ))
                    }
                }
            }
            Ok(entries)
        }
        _ => Err(lua_err("Inherits must be a ClassData or a list of parents")),
    }
}

fn merge_field(
    pmeta: &Table,
    kind: &str,
    want: &Option<Table>,
    all: bool,
    provided: &Table,
    target: &Table,
    label: &str,
) -> mlua::Result<()> {
    let source: Table = pmeta.raw_get(kind)?;
    if all {
        for pair in source.pairs::<Value, Value>() {
            let (k, v) = pair?;
            if !provided.raw_get::<Value>(k.clone())?.is_nil() {
                return Err(lua_err(format!(
                    "inherited {label} field '{}' is defined by more than one parent",
                    key_str(&k)
                )));
            }
            provided.raw_set(k.clone(), true)?;
            target.raw_set(k, v)?;
        }
    } else if let Some(list) = want {
        for entry in list.sequence_values::<Value>() {
            let k = entry?;
            let v: Value = source.raw_get(k.clone())?;
            if v.is_nil() {
                return Err(lua_err(format!(
                    "parent class has no {label} field '{}'",
                    key_str(&k)
                )));
            }
            if !provided.raw_get::<Value>(k.clone())?.is_nil() {
                return Err(lua_err(format!(
                    "inherited {label} field '{}' is defined by more than one parent",
                    key_str(&k)
                )));
            }
            provided.raw_set(k.clone(), true)?;
            target.raw_set(k, v)?;
        }
    }
    Ok(())
}

fn merge_statics(
    pmeta: &Table,
    want: &Option<Table>,
    all: bool,
    owners: &Table,
    own_static: &Table,
    provided: &Table,
) -> mlua::Result<()> {
    let mut keys: Vec<Value> = Vec::new();
    if all {
        let pown: Table = pmeta.raw_get("ownStatic")?;
        for pair in pown.pairs::<Value, Value>() {
            keys.push(pair?.0);
        }
        let powners: Table = pmeta.raw_get("staticOwners")?;
        for pair in powners.pairs::<Value, Value>() {
            keys.push(pair?.0);
        }
    } else if let Some(list) = want {
        for entry in list.sequence_values::<Value>() {
            keys.push(entry?);
        }
    }
    for k in keys {
        if !static_exists(pmeta, &k)? {
            return Err(lua_err(format!(
                "parent class has no static field '{}'",
                key_str(&k)
            )));
        }
        let dup = !provided.raw_get::<Value>(k.clone())?.is_nil()
            || !own_static.raw_get::<Value>(k.clone())?.is_nil();
        if dup {
            return Err(lua_err(format!(
                "inherited static field '{}' is defined by more than one parent",
                key_str(&k)
            )));
        }
        provided.raw_set(k.clone(), true)?;
        owners.raw_set(k.clone(), static_definer(pmeta, &k)?)?;
    }
    Ok(())
}

fn default_console(meta: &Table, internal: &Table) -> mlua::Result<String> {
    let public: Table = meta.raw_get("public")?;
    let mut parts: Vec<String> = Vec::new();
    for pair in public.pairs::<Value, Value>() {
        let (k, _) = pair?;
        let val: Value = internal.raw_get(k.clone())?;
        if let Value::Function(_) = val {
            continue;
        }
        parts.push(format!("{} = {}", key_str(&k), value_display(&val)));
    }
    parts.sort();
    let label = match meta.raw_get::<Value>("name")? {
        Value::String(s) => s.to_string_lossy(),
        _ => "Class".to_string(),
    };
    if parts.is_empty() {
        Ok(format!("{label} {{}}"))
    } else {
        Ok(format!("{label} {{ {} }}", parts.join(", ")))
    }
}

fn new_class_data(lua: &Lua, spec: Table) -> mlua::Result<Table> {
    let public = lua.create_table()?;
    let private = lua.create_table()?;
    let own_static = lua.create_table()?;
    let static_owners = lua.create_table()?;
    let metamethods = lua.create_table()?;
    let provided_public = lua.create_table()?;
    let provided_private = lua.create_table()?;
    let provided_static = lua.create_table()?;
    let provided_meta = lua.create_table()?;
    let parents = lua.create_table()?;

    let mut parent_index = 1;
    for entry in inherit_entries(&spec)? {
        let pmeta = class_meta(&entry.class)?;
        parents.raw_set(parent_index, entry.class.clone())?;
        parent_index += 1;
        merge_field(&pmeta, "public", &entry.public, entry.all, &provided_public, &public, "public")?;
        merge_field(&pmeta, "private", &entry.private, entry.all, &provided_private, &private, "private")?;
        merge_statics(&pmeta, &entry.statics, entry.all, &static_owners, &own_static, &provided_static)?;
        merge_field(&pmeta, "metamethods", &entry.metamethods, entry.all, &provided_meta, &metamethods, "metamethod")?;
    }

    if let Value::Table(p) = spec.raw_get::<Value>("Public")? {
        for pair in p.pairs::<Value, Value>() {
            let (k, v) = pair?;
            public.raw_set(k, v)?;
        }
    }
    if let Value::Table(p) = spec.raw_get::<Value>("Private")? {
        for pair in p.pairs::<Value, Value>() {
            let (k, v) = pair?;
            private.raw_set(k, v)?;
        }
    }
    if let Value::Table(p) = spec.raw_get::<Value>("Static")? {
        for pair in p.pairs::<Value, Value>() {
            let (k, v) = pair?;
            own_static.raw_set(k.clone(), v)?;
            static_owners.raw_set(k, Value::Nil)?;
        }
    }
    for pair in spec.pairs::<Value, Value>() {
        let (k, v) = pair?;
        if let Value::String(s) = &k {
            let name = s.to_string_lossy();
            if name.starts_with("__") && name != "__construct" {
                metamethods.raw_set(k.clone(), v)?;
            }
        }
    }

    let interfaces = lua.create_table()?;
    let class = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.raw_set("__classdata", true)?;
    meta.raw_set("public", public)?;
    meta.raw_set("private", private)?;
    meta.raw_set("ownStatic", own_static)?;
    meta.raw_set("staticOwners", static_owners)?;
    meta.raw_set("metamethods", metamethods)?;
    meta.raw_set("construct", spec.raw_get::<Value>("__construct")?)?;
    meta.raw_set("interfaces", interfaces.clone())?;
    meta.raw_set("parents", parents)?;
    meta.raw_set("name", spec.raw_get::<Value>("Name")?)?;
    meta.raw_set("classRef", class.clone())?;
    meta.raw_set("__type", "ClassData")?;

    let meta_index = meta.clone();
    meta.raw_set(
        "__index",
        lua.create_function(move |_, (_this, key): (Value, Value)| {
            resolve_static(&meta_index, &key)
        })?,
    )?;
    let meta_newindex = meta.clone();
    meta.raw_set(
        "__newindex",
        lua.create_function(move |_, (_this, key, value): (Value, Value, Value)| {
            let own_static: Table = meta_newindex.raw_get("ownStatic")?;
            if !own_static.raw_get::<Value>(key.clone())?.is_nil() {
                own_static.raw_set(key, value)?;
                return Ok(());
            }
            let owners: Table = meta_newindex.raw_get("staticOwners")?;
            if let Value::Table(owner_class) = owners.raw_get::<Value>(key.clone())? {
                let owner_meta = class_meta(&owner_class)?;
                let owner_static: Table = owner_meta.raw_get("ownStatic")?;
                owner_static.raw_set(key, value)?;
                return Ok(());
            }
            own_static.raw_set(key, value)?;
            Ok(())
        })?,
    )?;
    class.set_metatable(Some(meta.clone()))?;

    if let Value::Table(ifaces) = spec.raw_get::<Value>("Interfaces")? {
        let mut i = 1;
        for entry in ifaces.sequence_values::<Value>() {
            let iface = entry?;
            interfaces.raw_set(i, iface.clone())?;
            i += 1;
            check_interface(&iface, &meta)?;
        }
    }

    Ok(class)
}

fn build_class_data(lua: &Lua, registry: &Table, mut args: MultiValue) -> mlua::Result<Table> {
    let class = match args.pop_front() {
        Some(Value::Table(t)) if is_class_table(&t) => t,
        _ => return Err(lua_err("expected a ClassData value")),
    };
    let meta = class_meta(&class)?;
    let private: Table = meta.raw_get("private")?;
    let public: Table = meta.raw_get("public")?;

    let internal = lua.create_table()?;
    let mut seen: HashMap<usize, Table> = HashMap::new();
    for pair in private.pairs::<Value, Value>() {
        let (k, v) = pair?;
        internal.raw_set(k, deep_copy(lua, &v, &mut seen)?)?;
    }
    for pair in public.pairs::<Value, Value>() {
        let (k, v) = pair?;
        internal.raw_set(k, deep_copy(lua, &v, &mut seen)?)?;
    }
    let internal_mt = lua.create_table()?;
    let meta_for_internal = meta.clone();
    internal_mt.raw_set(
        "__index",
        lua.create_function(move |_, (_t, key): (Value, Value)| {
            resolve_static(&meta_for_internal, &key)
        })?,
    )?;
    internal.set_metatable(Some(internal_mt))?;

    let proxy = lua.create_table()?;
    let bound_cache = lua.create_table()?;
    let pmeta = lua.create_table()?;
    pmeta.raw_set("classMeta", meta.clone())?;

    let idx_meta = meta.clone();
    let idx_internal = internal.clone();
    let idx_cache = bound_cache.clone();
    let idx_registry = registry.clone();
    pmeta.raw_set(
        "__index",
        lua.create_function(move |lua, (_t, key): (Value, Value)| {
            if static_exists(&idx_meta, &key)? {
                return resolve_static(&idx_meta, &key);
            }
            let public: Table = idx_meta.raw_get("public")?;
            if !public.raw_get::<Value>(key.clone())?.is_nil() {
                let val: Value = idx_internal.raw_get(key.clone())?;
                if let Value::Function(f) = val {
                    let cached: Value = idx_cache.raw_get(key.clone())?;
                    if !cached.is_nil() {
                        return Ok(cached);
                    }
                    let bound = Value::Function(bind_super(lua, &idx_registry, f)?);
                    idx_cache.raw_set(key, bound.clone())?;
                    return Ok(bound);
                }
                return Ok(val);
            }
            let private: Table = idx_meta.raw_get("private")?;
            if !private.raw_get::<Value>(key.clone())?.is_nil() {
                return Err(lua_err(format!(
                    "cannot read private field '{}' from outside the class",
                    key_str(&key)
                )));
            }
            Ok(Value::Nil)
        })?,
    )?;

    let ni_meta = meta.clone();
    let ni_internal = internal.clone();
    pmeta.raw_set(
        "__newindex",
        lua.create_function(move |_, (_t, key, value): (Value, Value, Value)| {
            let public: Table = ni_meta.raw_get("public")?;
            if !public.raw_get::<Value>(key.clone())?.is_nil() {
                ni_internal.raw_set(key, value)?;
                return Ok(());
            }
            if static_exists(&ni_meta, &key)? {
                if let Value::Table(owner) = static_definer(&ni_meta, &key)? {
                    let owner_meta = class_meta(&owner)?;
                    let owner_static: Table = owner_meta.raw_get("ownStatic")?;
                    owner_static.raw_set(key, value)?;
                }
                return Ok(());
            }
            let private: Table = ni_meta.raw_get("private")?;
            if !private.raw_get::<Value>(key.clone())?.is_nil() {
                return Err(lua_err(format!(
                    "cannot assign to private field '{}' from outside the class",
                    key_str(&key)
                )));
            }
            Err(lua_err(format!(
                "cannot assign to undeclared field '{}'",
                key_str(&key)
            )))
        })?,
    )?;

    let metamethods: Table = meta.raw_get("metamethods")?;
    for pair in metamethods.pairs::<Value, Value>() {
        let (name_v, fn_v) = pair?;
        let name = match &name_v {
            Value::String(s) => s.to_string_lossy(),
            _ => continue,
        };
        match name.as_str() {
            "__type" | "__toconsole" => {
                if let Value::Function(f) = fn_v {
                    let bound_internal = internal.clone();
                    pmeta.raw_set(
                        name_v.clone(),
                        lua.create_function(move |_, ()| {
                            f.call::<Value>(Value::Table(bound_internal.clone()))
                        })?,
                    )?;
                } else {
                    pmeta.raw_set(name_v.clone(), fn_v)?;
                }
            }
            "__index" | "__newindex" => {}
            _ => {
                if let Value::Function(f) = fn_v {
                    let reg = registry.clone();
                    pmeta.raw_set(
                        name_v.clone(),
                        lua.create_function(move |_, call: MultiValue| {
                            let mut it = call.into_iter();
                            let a = it.next().unwrap_or(Value::Nil);
                            let b = it.next().unwrap_or(Value::Nil);
                            let mut with = Vec::new();
                            with.push(translate(&reg, a)?);
                            with.push(translate(&reg, b)?);
                            for extra in it {
                                with.push(extra);
                            }
                            f.call::<MultiValue>(MultiValue::from_vec(with))
                        })?,
                    )?;
                } else {
                    pmeta.raw_set(name_v.clone(), fn_v)?;
                }
            }
        }
    }

    if pmeta.raw_get::<Value>("__toconsole")?.is_nil() {
        let console_meta = meta.clone();
        let console_internal = internal.clone();
        pmeta.raw_set(
            "__toconsole",
            lua.create_function(move |_, ()| default_console(&console_meta, &console_internal))?,
        )?;
    }

    registry.raw_set(proxy.clone(), internal.clone())?;
    proxy.set_metatable(Some(pmeta))?;

    if let Value::Function(construct) = meta.raw_get::<Value>("construct")? {
        let mut call_args = Vec::with_capacity(args.len() + 1);
        call_args.push(Value::Table(internal.clone()));
        call_args.extend(args.into_iter());
        construct.call::<MultiValue>(MultiValue::from_vec(call_args))?;
    }

    Ok(proxy)
}

fn super_member(meta: &Table, key: &Value) -> mlua::Result<Option<Value>> {
    let public: Table = meta.raw_get("public")?;
    let v: Value = public.raw_get(key.clone())?;
    if !v.is_nil() {
        return Ok(Some(v));
    }
    let private: Table = meta.raw_get("private")?;
    let v: Value = private.raw_get(key.clone())?;
    if !v.is_nil() {
        return Ok(Some(v));
    }
    if static_exists(meta, key)? {
        return Ok(Some(resolve_static(meta, key)?));
    }
    let mm: Table = meta.raw_get("metamethods")?;
    let v: Value = mm.raw_get(key.clone())?;
    if !v.is_nil() {
        return Ok(Some(v));
    }
    Ok(None)
}

fn bind_super(lua: &Lua, registry: &Table, f: Function) -> mlua::Result<Function> {
    let reg = registry.clone();
    lua.create_function(move |_, call: MultiValue| {
        let mut args = Vec::with_capacity(call.len());
        for a in call.into_iter() {
            args.push(translate(&reg, a)?);
        }
        f.call::<MultiValue>(MultiValue::from_vec(args))
    })
}

fn super_get(lua: &Lua, registry: &Table, class: Value, key: Value) -> mlua::Result<Value> {
    let class = match class {
        Value::Table(t) if is_class_table(&t) => t,
        _ => return Err(lua_err("expected a ClassData value")),
    };
    let meta = class_meta(&class)?;
    match super_member(&meta, &key)? {
        Some(Value::Function(f)) => Ok(Value::Function(bind_super(lua, registry, f)?)),
        Some(v) => Ok(v),
        None => Err(lua_err(format!("SuperGet: class has no field '{}'", key_str(&key)))),
    }
}

fn interface(lua: &Lua, spec: Table) -> mlua::Result<Table> {
    let name = match spec.raw_get::<Value>("Name")? {
        Value::Nil => Value::String(lua.create_string("Interface")?),
        other => other,
    };
    let list = match spec.raw_get::<Value>("Requires")? {
        Value::Table(t) => t,
        _ => spec.clone(),
    };
    let requires = lua.create_table()?;
    let mut i = 1;
    for entry in list.sequence_values::<Value>() {
        requires.raw_set(i, entry?)?;
        i += 1;
    }
    let iface = lua.create_table()?;
    let imeta = lua.create_table()?;
    imeta.raw_set("__interface", true)?;
    imeta.raw_set("requires", requires)?;
    imeta.raw_set("name", name)?;
    imeta.raw_set("__type", "Interface")?;
    iface.set_metatable(Some(imeta))?;
    Ok(iface)
}

fn implements(registry: &Table, obj: Value, iface: Value) -> mlua::Result<bool> {
    let imeta = interface_meta(&iface)?;
    let requires: Table = imeta.raw_get("requires")?;

    if let Value::Table(t) = &obj {
        if is_class_table(t) {
            let meta = class_meta(t)?;
            for entry in requires.sequence_values::<Value>() {
                if !surface_has(&meta, &entry?)? {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        if let Value::Table(_) = registry.raw_get::<Value>(t.clone())? {
            if let Some(pmeta) = t.metatable() {
                let class_meta_ref: Table = pmeta.raw_get("classMeta")?;
                for entry in requires.sequence_values::<Value>() {
                    if !surface_has(&class_meta_ref, &entry?)? {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
        }
        for entry in requires.sequence_values::<Value>() {
            if t.get::<Value>(entry?)?.is_nil() {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    Ok(false)
}

pub fn install(lua: &Lua) -> mlua::Result<()> {
    let registry = lua.create_table()?;
    let registry_mt = lua.create_table()?;
    registry_mt.raw_set("__mode", "k")?;
    registry.set_metatable(Some(registry_mt))?;

    let globals = lua.globals();

    globals.set(
        "NewClassData",
        lua.create_function(|lua, spec: Value| match spec {
            Value::Table(t) => new_class_data(lua, t),
            _ => Err(lua_err("NewClassData expects a table")),
        })?,
    )?;

    let build_registry = registry.clone();
    globals.set(
        "BuildClassData",
        lua.create_function(move |lua, args: MultiValue| {
            build_class_data(lua, &build_registry, args)
        })?,
    )?;

    let super_registry = registry.clone();
    globals.set(
        "SuperGet",
        lua.create_function(move |lua, (class, key): (Value, Value)| {
            super_get(lua, &super_registry, class, key)
        })?,
    )?;

    globals.set(
        "Interface",
        lua.create_function(|lua, spec: Value| match spec {
            Value::Table(t) => interface(lua, t),
            _ => Err(lua_err("Interface expects a table")),
        })?,
    )?;

    let implements_registry = registry.clone();
    globals.set(
        "Implements",
        lua.create_function(move |_, (obj, iface): (Value, Value)| {
            implements(&implements_registry, obj, iface)
        })?,
    )?;

    let raw_typeof: Function = globals.get("typeof")?;
    globals.set(
        "typeof",
        lua.create_function(move |_, v: Value| {
            let marker = match &v {
                Value::Table(t) => {
                    let direct: Value = t.raw_get("__type")?;
                    if direct.is_nil() {
                        match t.metatable() {
                            Some(mt) => mt.raw_get::<Value>("__type")?,
                            None => Value::Nil,
                        }
                    } else {
                        direct
                    }
                }
                _ => Value::Nil,
            };
            match marker {
                Value::Nil => raw_typeof.call::<Value>(v),
                Value::Function(f) => f.call::<Value>(v),
                other => Ok(other),
            }
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> Lua {
        let lua = Lua::new();
        install(&lua).unwrap();
        lua
    }

    fn eval_bool(src: &str) -> bool {
        vm().load(src).eval().unwrap()
    }

    fn eval_i64(src: &str) -> i64 {
        vm().load(src).eval().unwrap()
    }

    fn eval_string(src: &str) -> String {
        vm().load(src).eval().unwrap()
    }

    #[test]
    fn builds_instance_and_runs_methods() {
        assert_eq!(
            eval_i64(
                r#"
                local C = NewClassData({
                    Private = { n = 0 },
                    Public = { Inc = function(self) self.n = self.n + 1 return self.n end },
                    __construct = function(self, start) self.n = start end,
                })
                local a = BuildClassData(C, 5)
                return a:Inc() + a:Inc()
                "#
            ),
            13
        );
    }

    #[test]
    fn static_is_accessible_from_class_root() {
        assert_eq!(
            eval_string(
                r#"
                local C = NewClassData({ Static = { Version = "1.0", Tag = function() return "t" end }, Public = {} })
                return C.Version .. ":" .. C.Tag()
                "#
            ),
            "1.0:t"
        );
    }

    #[test]
    fn reading_private_from_outside_errors() {
        assert!(!eval_bool(
            r#"
            local C = NewClassData({ Private = { secret = 1 }, Public = {} })
            local a = BuildClassData(C)
            return (pcall(function() return a.secret end))
            "#
        ));
    }

    #[test]
    fn writing_private_from_outside_errors() {
        assert!(!eval_bool(
            r#"
            local C = NewClassData({ Private = { secret = 1 }, Public = {} })
            local a = BuildClassData(C)
            return (pcall(function() a.secret = 2 end))
            "#
        ));
    }

    #[test]
    fn instances_have_independent_private_state() {
        assert!(eval_bool(
            r#"
            local C = NewClassData({
                Private = { items = { "x" } },
                Public = {
                    Add = function(self, v) table.insert(self.items, v) end,
                    Count = function(self) return #self.items end,
                },
            })
            local a = BuildClassData(C)
            local b = BuildClassData(C)
            a:Add("y")
            return a:Count() == 2 and b:Count() == 1
            "#
        ));
    }

    #[test]
    fn single_inheritance_and_override() {
        assert_eq!(
            eval_string(
                r#"
                local Animal = NewClassData({
                    Private = { name = "?" },
                    Public = {
                        Speak = function(self) return self.name .. " sound" end,
                        Name = function(self) return self.name end,
                    },
                    __construct = function(self, n) self.name = n end,
                })
                local Dog = NewClassData({
                    Inherits = Animal,
                    Public = { Speak = function(self) return self.name .. " bark" end },
                    __construct = function(self, n) self.name = n end,
                })
                local d = BuildClassData(Dog, "Rex")
                return d:Speak() .. "/" .. d:Name()
                "#
            ),
            "Rex bark/Rex"
        );
    }

    #[test]
    fn duplicate_inherited_name_errors() {
        assert!(!eval_bool(
            r#"
            local A = NewClassData({ Public = { foo = function() return 1 end } })
            local B = NewClassData({ Public = { foo = function() return 2 end } })
            return (pcall(function()
                return NewClassData({
                    Inherits = {
                        { Class = A, Public = { "foo" } },
                        { Class = B, Public = { "foo" } },
                    },
                })
            end))
            "#
        ));
    }

    #[test]
    fn inheriting_missing_key_errors() {
        assert!(!eval_bool(
            r#"
            local A = NewClassData({ Public = { foo = function() return 1 end } })
            return (pcall(function()
                return NewClassData({ Inherits = { { Class = A, Public = { "missing" } } } })
            end))
            "#
        ));
    }

    #[test]
    fn statics_resolve_back_to_defining_class() {
        assert_eq!(
            eval_string(
                r#"
                local Base = NewClassData({ Static = { Tag = "base" }, Public = {} })
                local Child = NewClassData({ Inherits = { { Class = Base, Static = { "Tag" } } }, Public = {} })
                Base.Tag = "changed"
                return tostring(Child.Tag)
                "#
            ),
            "changed"
        );
    }

    #[test]
    fn super_get_reaches_uninherited_field() {
        assert_eq!(
            eval_string(
                r#"
                local Base = NewClassData({
                    Private = { name = "b" },
                    Public = { Hidden = function(self) return "hidden:" .. self.name end },
                })
                local Child = NewClassData({
                    Inherits = { { Class = Base, Private = { "name" } } },
                    Public = {
                        Reach = function(self) return SuperGet(Base, "Hidden")(self) end,
                    },
                    __construct = function(self) self.name = "c" end,
                })
                local c = BuildClassData(Child)
                return c:Reach()
                "#
            ),
            "hidden:c"
        );
    }

    #[test]
    fn typeof_is_native_for_non_tables() {
        assert_eq!(
            eval_string(
                r#"
                local co = coroutine.create(function() end)
                local parts = {
                    typeof(nil),
                    typeof(true),
                    typeof(1),
                    typeof(1.5),
                    typeof("s"),
                    typeof(print),
                    typeof(co),
                    typeof(vector.create(1, 2, 3)),
                    typeof(buffer.create(4)),
                }
                return table.concat(parts, ",")
                "#
            ),
            "nil,boolean,number,number,string,function,thread,vector,buffer"
        );
    }

    #[test]
    fn typeof_is_table_for_plain_table_with_metatable() {
        assert_eq!(
            eval_string(
                r#"
                local t = setmetatable({}, { __index = function() end })
                return typeof(t)
                "#
            ),
            "table"
        );
    }

    #[test]
    fn typeof_uses_type_marker() {
        assert_eq!(
            eval_string(
                r#"
                local C = NewClassData({ Public = {}, __type = "Widget", __construct = function() end })
                local a = BuildClassData(C)
                local F = NewClassData({
                    Private = { t = "x" },
                    Public = {},
                    __type = function(self) return "Tag:" .. self.t end,
                    __construct = function(self, t) self.t = t end,
                })
                local f = BuildClassData(F, "z")
                return typeof(a) .. "," .. typeof(f) .. "," .. typeof({}) .. "," .. typeof(5)
                "#
            ),
            "Widget,Tag:z,table,number"
        );
    }

    #[test]
    fn interface_validation_and_implements() {
        assert!(eval_bool(
            r#"
            local IShape = Interface({ Name = "IShape", Requires = { "Area" } })
            local bad = pcall(function()
                return NewClassData({ Public = {}, Interfaces = { IShape } })
            end)
            local Circle = NewClassData({
                Private = { r = 0 },
                Public = { Area = function(self) return self.r * self.r end },
                Interfaces = { IShape },
                __construct = function(self, r) self.r = r end,
            })
            local c = BuildClassData(Circle, 3)
            return (not bad) and Implements(c, IShape) and (not Implements(c, Interface({ "Nope" })))
            "#
        ));
    }

    #[test]
    fn metamethod_operands_translate_to_internal() {
        assert_eq!(
            eval_i64(
                r#"
                local Vec
                Vec = NewClassData({
                    Private = { x = 0 },
                    Public = { Get = function(self) return self.x end },
                    __construct = function(self, x) self.x = x end,
                    __add = function(self, other) return BuildClassData(Vec, self.x + other.x) end,
                })
                local a = BuildClassData(Vec, 4)
                local b = BuildClassData(Vec, 6)
                return (a + b):Get()
                "#
            ),
            10
        );
    }
}
