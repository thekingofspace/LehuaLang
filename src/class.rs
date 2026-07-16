use std::collections::HashMap;
use std::rc::Rc;

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

const CLASS_HELPERS: &str = r#"
local translateAll, translateOne = ...
local function bind(f)
    return function(...)
        return f(translateAll(...))
    end
end
local function metamethod(f)
    return function(a, b, ...)
        return f(translateOne(a), translateOne(b), ...)
    end
end
local function builder(build)
    return function(...)
        local proxy, construct, internal = build(...)
        if construct then
            construct(internal, select(2, ...))
        end
        return proxy
    end
end
return bind, metamethod, builder
"#;

struct Helpers {
    bind_factory: Function,
    metamethod_factory: Function,
    builder: Function,
    bind_cache: Table,
    metamethod_cache: Table,
}

impl Helpers {
    fn bind(&self, f: Function) -> mlua::Result<Function> {
        if let Value::Function(cached) = self.bind_cache.raw_get::<Value>(f.clone())? {
            return Ok(cached);
        }
        let wrapper: Function = self.bind_factory.call(f.clone())?;
        self.bind_cache.raw_set(f, wrapper.clone())?;
        Ok(wrapper)
    }

    fn metamethod(&self, f: Function) -> mlua::Result<Function> {
        if let Value::Function(cached) = self.metamethod_cache.raw_get::<Value>(f.clone())? {
            return Ok(cached);
        }
        let wrapper: Function = self.metamethod_factory.call(f.clone())?;
        self.metamethod_cache.raw_set(f, wrapper.clone())?;
        Ok(wrapper)
    }
}

fn weak_key_table(lua: &Lua) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.raw_set("__mode", "k")?;
    t.set_metatable(Some(mt))?;
    Ok(t)
}

fn make_helpers(lua: &Lua, registry: &Table) -> mlua::Result<Rc<Helpers>> {
    let reg_all = registry.clone();
    let translate_all = lua.create_function(move |_, args: MultiValue| {
        let mut out = Vec::with_capacity(args.len());
        for a in args.into_iter() {
            out.push(translate(&reg_all, a)?);
        }
        Ok(MultiValue::from_vec(out))
    })?;
    let reg_one = registry.clone();
    let translate_one = lua.create_function(move |_, v: Value| translate(&reg_one, v))?;
    let (bind_factory, metamethod_factory, builder): (Function, Function, Function) = lua
        .load(CLASS_HELPERS)
        .set_name("@lehua/class")
        .call((translate_all, translate_one))?;
    Ok(Rc::new(Helpers {
        bind_factory,
        metamethod_factory,
        builder,
        bind_cache: weak_key_table(lua)?,
        metamethod_cache: weak_key_table(lua)?,
    }))
}

fn class_label(meta: &Table) -> String {
    match meta.raw_get::<Value>("name") {
        Ok(Value::String(s)) => format!(" of class '{}'", s.to_string_lossy()),
        _ => String::new(),
    }
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

fn translated_internal(registry: &Table, v: Value) -> mlua::Result<Table> {
    match translate(registry, v)? {
        Value::Table(t) => Ok(t),
        _ => Err(lua_err("expected a class object")),
    }
}

fn proxy_meta(
    lua: &Lua,
    registry: &Table,
    helpers: &Rc<Helpers>,
    meta: &Table,
) -> mlua::Result<Table> {
    if let Value::Table(existing) = meta.raw_get::<Value>("proxyMeta")? {
        return Ok(existing);
    }
    let pmeta = lua.create_table()?;
    pmeta.raw_set("classMeta", meta.clone())?;

    let idx_meta = meta.clone();
    let idx_registry = registry.clone();
    let idx_helpers = helpers.clone();
    pmeta.raw_set(
        "__index",
        lua.create_function(move |_, (this, key): (Value, Value)| {
            if static_exists(&idx_meta, &key)? {
                return resolve_static(&idx_meta, &key);
            }
            let public: Table = idx_meta.raw_get("public")?;
            if !public.raw_get::<Value>(key.clone())?.is_nil() {
                let internal = translated_internal(&idx_registry, this)?;
                let val: Value = internal.raw_get(key)?;
                if let Value::Function(f) = val {
                    return Ok(Value::Function(idx_helpers.bind(f)?));
                }
                return Ok(val);
            }
            let private: Table = idx_meta.raw_get("private")?;
            if !private.raw_get::<Value>(key.clone())?.is_nil() {
                return Err(lua_err(format!(
                    "cannot read private field '{}'{} from outside the class",
                    key_str(&key),
                    class_label(&idx_meta)
                )));
            }
            Ok(Value::Nil)
        })?,
    )?;

    let ni_meta = meta.clone();
    let ni_registry = registry.clone();
    pmeta.raw_set(
        "__newindex",
        lua.create_function(move |_, (this, key, value): (Value, Value, Value)| {
            let public: Table = ni_meta.raw_get("public")?;
            if !public.raw_get::<Value>(key.clone())?.is_nil() {
                let internal = translated_internal(&ni_registry, this)?;
                internal.raw_set(key, value)?;
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
                    "cannot assign to private field '{}'{} from outside the class",
                    key_str(&key),
                    class_label(&ni_meta)
                )));
            }
            Err(lua_err(format!(
                "cannot assign to undeclared field '{}'{}",
                key_str(&key),
                class_label(&ni_meta)
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
                    let reg = registry.clone();
                    pmeta.raw_set(
                        name_v.clone(),
                        lua.create_function(move |_, v: Value| {
                            f.call::<Value>(Value::Table(translated_internal(&reg, v)?))
                        })?,
                    )?;
                } else {
                    pmeta.raw_set(name_v.clone(), fn_v)?;
                }
            }
            "__index" | "__newindex" => {}
            _ => {
                if let Value::Function(f) = fn_v {
                    pmeta.raw_set(name_v.clone(), helpers.metamethod(f)?)?;
                } else {
                    pmeta.raw_set(name_v.clone(), fn_v)?;
                }
            }
        }
    }

    if pmeta.raw_get::<Value>("__type")?.is_nil() {
        if let Value::String(name) = meta.raw_get::<Value>("name")? {
            pmeta.raw_set("__type", name)?;
        }
    }

    if pmeta.raw_get::<Value>("__toconsole")?.is_nil() {
        let console_meta = meta.clone();
        let console_registry = registry.clone();
        pmeta.raw_set(
            "__toconsole",
            lua.create_function(move |_, v: Value| {
                default_console(&console_meta, &translated_internal(&console_registry, v)?)
            })?,
        )?;
    }

    if pmeta.raw_get::<Value>("__tostring")?.is_nil() {
        let string_meta = meta.clone();
        let string_registry = registry.clone();
        pmeta.raw_set(
            "__tostring",
            lua.create_function(move |_, v: Value| {
                default_console(&string_meta, &translated_internal(&string_registry, v)?)
            })?,
        )?;
    }

    meta.raw_set("proxyMeta", pmeta.clone())?;
    Ok(pmeta)
}

fn internal_meta(lua: &Lua, meta: &Table) -> mlua::Result<Table> {
    if let Value::Table(existing) = meta.raw_get::<Value>("internalMeta")? {
        return Ok(existing);
    }
    let mt = lua.create_table()?;
    let m = meta.clone();
    mt.raw_set(
        "__index",
        lua.create_function(move |_, (_t, key): (Value, Value)| resolve_static(&m, &key))?,
    )?;
    meta.raw_set("internalMeta", mt.clone())?;
    Ok(mt)
}

fn build_class_data(
    lua: &Lua,
    registry: &Table,
    helpers: &Rc<Helpers>,
    mut args: MultiValue,
) -> mlua::Result<(Table, Value, Table)> {
    let class = match args.pop_front() {
        Some(Value::Table(t)) if is_class_table(&t) => t,
        _ => return Err(lua_err("expected a ClassData value")),
    };
    let meta = class_meta(&class)?;
    let pmeta = proxy_meta(lua, registry, helpers, &meta)?;
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
    internal.set_metatable(Some(internal_meta(lua, &meta)?))?;

    let proxy = lua.create_table()?;
    registry.raw_set(proxy.clone(), internal.clone())?;
    proxy.set_metatable(Some(pmeta))?;

    let construct: Value = meta.raw_get("construct")?;
    let construct = match construct {
        Value::Function(f) => Value::Function(f),
        _ => Value::Nil,
    };

    Ok((proxy, construct, internal))
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

fn super_get(helpers: &Rc<Helpers>, class: Value, key: Value) -> mlua::Result<Value> {
    let class = match class {
        Value::Table(t) if is_class_table(&t) => t,
        _ => return Err(lua_err("expected a ClassData value")),
    };
    let meta = class_meta(&class)?;
    match super_member(&meta, &key)? {
        Some(Value::Function(f)) => Ok(Value::Function(helpers.bind(f)?)),
        Some(v) => Ok(v),
        None => Err(lua_err(format!("SuperGet: class has no field '{}'", key_str(&key)))),
    }
}

fn class_of(registry: &Table, v: &Value) -> mlua::Result<Option<Table>> {
    if let Value::Table(t) = v {
        if is_class_table(t) {
            return Ok(Some(t.clone()));
        }
        if let Value::Table(_) = registry.raw_get::<Value>(t.clone())? {
            if let Some(pmeta) = t.metatable() {
                if let Ok(cm) = pmeta.raw_get::<Table>("classMeta") {
                    return Ok(Some(cm.raw_get("classRef")?));
                }
            }
        }
    }
    Ok(None)
}

fn is_a(registry: &Table, obj: Value, class: Value) -> mlua::Result<bool> {
    let target = match &class {
        Value::Table(t) if is_class_table(t) => t.to_pointer() as usize,
        _ => return Err(lua_err("IsA expects a ClassData as its second argument")),
    };
    let Some(start) = class_of(registry, &obj)? else {
        return Ok(false);
    };
    let mut stack = vec![start];
    let mut seen: Vec<usize> = Vec::new();
    while let Some(c) = stack.pop() {
        let ptr = c.to_pointer() as usize;
        if ptr == target {
            return Ok(true);
        }
        if seen.contains(&ptr) {
            continue;
        }
        seen.push(ptr);
        let meta = class_meta(&c)?;
        let parents: Table = meta.raw_get("parents")?;
        for p in parents.sequence_values::<Table>() {
            stack.push(p?);
        }
    }
    Ok(false)
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

    let helpers = make_helpers(lua, &registry)?;
    let globals = lua.globals();

    globals.set(
        "NewClassData",
        lua.create_function(|lua, spec: Value| match spec {
            Value::Table(t) => new_class_data(lua, t),
            _ => Err(lua_err("NewClassData expects a table")),
        })?,
    )?;

    let build_registry = registry.clone();
    let build_helpers = helpers.clone();
    let build_fn = lua.create_function(move |lua, args: MultiValue| {
        build_class_data(lua, &build_registry, &build_helpers, args)
    })?;
    let build_global: Function = helpers.builder.call(build_fn)?;
    globals.set("BuildClassData", build_global)?;

    let super_helpers = helpers.clone();
    globals.set(
        "SuperGet",
        lua.create_function(move |_, (class, key): (Value, Value)| {
            super_get(&super_helpers, class, key)
        })?,
    )?;

    let isa_registry = registry.clone();
    globals.set(
        "IsA",
        lua.create_function(move |_, (obj, class): (Value, Value)| {
            is_a(&isa_registry, obj, class)
        })?,
    )?;

    let fetch_registry = registry.clone();
    globals.set(
        "FetchClass",
        lua.create_function(move |_, v: Value| {
            Ok(match class_of(&fetch_registry, &v)? {
                Some(c) => Value::Table(c),
                None => Value::Nil,
            })
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

    let native_typeof: Function = globals.get("typeof")?;
    globals.set(
        "GetType",
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
                Value::Nil => native_typeof.call::<Value>(v),
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
    fn get_type_is_native_for_non_tables() {
        assert_eq!(
            eval_string(
                r#"
                local co = coroutine.create(function() end)
                local parts = {
                    GetType(nil),
                    GetType(true),
                    GetType(1),
                    GetType(1.5),
                    GetType("s"),
                    GetType(print),
                    GetType(co),
                    GetType(vector.create(1, 2, 3)),
                    GetType(buffer.create(4)),
                }
                return table.concat(parts, ",")
                "#
            ),
            "nil,boolean,number,number,string,function,thread,vector,buffer"
        );
    }

    #[test]
    fn get_type_is_table_for_plain_table_with_metatable() {
        assert_eq!(
            eval_string(
                r#"
                local t = setmetatable({}, { __index = function() end })
                return GetType(t)
                "#
            ),
            "table"
        );
    }

    #[test]
    fn get_type_uses_type_marker() {
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
                return GetType(a) .. "," .. GetType(f) .. "," .. GetType({}) .. "," .. GetType(5)
                "#
            ),
            "Widget,Tag:z,table,number"
        );
    }

    #[test]
    fn typeof_is_left_native() {
        assert_eq!(
            eval_string(
                r#"
                local C = NewClassData({ Public = {}, __type = "Widget", __construct = function() end })
                local a = BuildClassData(C)
                return typeof(a) .. "," .. typeof(C)
                "#
            ),
            "table,table"
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
    fn methods_can_yield_to_their_coroutine() {
        assert_eq!(
            eval_string(
                r#"
                local C = NewClassData({
                    Private = { n = 0 },
                    Public = {
                        Gen = function(self, first)
                            local got = coroutine.yield(first + self.n)
                            return got .. ":" .. self.n
                        end,
                    },
                    __construct = function(self, n) self.n = n end,
                })
                local a = BuildClassData(C, 7)
                local co = coroutine.create(function(x) return a:Gen(x) end)
                local ok1, v1 = coroutine.resume(co, 3)
                local ok2, v2 = coroutine.resume(co, "back")
                assert(ok1 and ok2, tostring(v1) .. "/" .. tostring(v2))
                return v1 .. "," .. v2
                "#
            ),
            "10,back:7"
        );
    }

    #[test]
    fn constructors_can_yield_to_their_coroutine() {
        assert_eq!(
            eval_string(
                r#"
                local C = NewClassData({
                    Private = { v = "" },
                    Public = { Get = function(self) return self.v end },
                    __construct = function(self, base)
                        self.v = base .. coroutine.yield("need-more")
                    end,
                })
                local co = coroutine.create(function()
                    local obj = BuildClassData(C, "a")
                    return obj:Get()
                end)
                local ok1, ask = coroutine.resume(co)
                local ok2, out = coroutine.resume(co, "b")
                assert(ok1 and ok2, tostring(ask) .. "/" .. tostring(out))
                return ask .. "," .. out
                "#
            ),
            "need-more,ab"
        );
    }

    #[test]
    fn superget_bound_functions_can_yield() {
        assert_eq!(
            eval_i64(
                r#"
                local Base = NewClassData({
                    Private = { n = 5 },
                    Public = {
                        Slow = function(self)
                            coroutine.yield()
                            return self.n
                        end,
                    },
                })
                local Child = NewClassData({
                    Inherits = { { Class = Base, Private = { "n" } } },
                    Public = {
                        Go = function(self) return SuperGet(Base, "Slow")(self) end,
                    },
                })
                local c = BuildClassData(Child)
                local co = coroutine.create(function() return c:Go() end)
                coroutine.resume(co)
                local ok, v = coroutine.resume(co)
                assert(ok, tostring(v))
                return v
                "#
            ),
            5
        );
    }

    #[test]
    fn is_a_walks_the_parent_graph() {
        assert!(eval_bool(
            r#"
            local A = NewClassData({ Name = "A", Public = {} })
            local B = NewClassData({ Name = "B", Inherits = A, Public = {} })
            local C = NewClassData({ Name = "C", Inherits = { { Class = B } }, Public = {} })
            local Other = NewClassData({ Name = "Other", Public = {} })
            local c = BuildClassData(C)
            return IsA(c, C) and IsA(c, B) and IsA(c, A)
                and IsA(C, A) and IsA(B, B)
                and not IsA(c, Other) and not IsA(A, B)
                and not IsA({}, A) and not IsA(5, A) and not IsA(nil, A)
            "#
        ));
    }

    #[test]
    fn is_a_rejects_non_class_target() {
        assert!(!eval_bool(
            r#"
            local A = NewClassData({ Public = {} })
            local a = BuildClassData(A)
            return (pcall(function() return IsA(a, {}) end))
            "#
        ));
    }

    #[test]
    fn named_classes_default_their_instance_type() {
        assert_eq!(
            eval_string(
                r#"
                local Widget = NewClassData({ Name = "Widget", Public = {} })
                local Anon = NewClassData({ Public = {} })
                local Custom = NewClassData({ Name = "Ignored", __type = "Custom", Public = {} })
                return GetType(BuildClassData(Widget)) .. ","
                    .. GetType(BuildClassData(Anon)) .. ","
                    .. GetType(BuildClassData(Custom))
                "#
            ),
            "Widget,table,Custom"
        );
    }

    #[test]
    fn tostring_defaults_to_console_repr() {
        assert_eq!(
            eval_string(
                r#"
                local P = NewClassData({
                    Name = "Point",
                    Public = { x = 0, y = 0 },
                    __construct = function(self, x, y) self.x = x self.y = y end,
                })
                return tostring(BuildClassData(P, 1, 2))
                "#
            ),
            "Point { x = 1, y = 2 }"
        );
    }

    #[test]
    fn private_errors_name_the_class() {
        assert!(eval_bool(
            r#"
            local C = NewClassData({ Name = "Vault", Private = { secret = 1 }, Public = {} })
            local a = BuildClassData(C)
            local ok, err = pcall(function() return a.secret end)
            return not ok and string.find(tostring(err), "Vault", 1, true) ~= nil
            "#
        ));
    }

    #[test]
    fn fetch_class_returns_the_classdata() {
        assert!(eval_bool(
            r#"
            local C = NewClassData({ Name = "C", Public = {} })
            local D = NewClassData({ Name = "D", Inherits = C, Public = {} })
            local d = BuildClassData(D)
            return FetchClass(d) == D and FetchClass(D) == D
                and FetchClass({}) == nil and FetchClass(5) == nil and FetchClass(nil) == nil
                and IsA(FetchClass(d), C)
            "#
        ));
    }

    #[test]
    fn instances_share_bound_methods_and_metatables() {
        assert!(eval_bool(
            r#"
            local C = NewClassData({
                Private = { n = 0 },
                Public = { Inc = function(self) self.n += 1 return self.n end },
            })
            local a = BuildClassData(C)
            local b = BuildClassData(C)
            return rawequal(a.Inc, b.Inc)
                and rawequal(getmetatable(a), getmetatable(b))
                and a:Inc() == 1 and a:Inc() == 2 and b:Inc() == 1
            "#
        ));
    }

    #[test]
    fn eq_metamethod_fires_between_instances() {
        assert!(eval_bool(
            r#"
            local V = NewClassData({
                Private = { x = 0 },
                Public = {},
                __eq = function(self, other) return self.x == other.x end,
                __construct = function(self, x) self.x = x end,
            })
            local a = BuildClassData(V, 3)
            local b = BuildClassData(V, 3)
            local c = BuildClassData(V, 4)
            return a == b and not (a == c)
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
