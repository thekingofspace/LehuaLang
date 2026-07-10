use std::collections::HashSet;

use mlua::{Lua, Value};

use crate::error::{LehuaError, Result};

const MAX_DEPTH: usize = 96;

#[derive(Debug, Clone)]
pub enum PortableValue {
    Nil,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(Vec<u8>),
    Array(Vec<PortableValue>),
    Map(Vec<(PortableValue, PortableValue)>),
    #[cfg(feature = "lib-cache")]
    MemCache(String),
}

impl PortableValue {
    pub fn from_lua(v: &Value) -> Result<Self> {
        let mut seen = HashSet::new();
        Self::from_lua_inner(v, 0, &mut seen)
    }

    fn from_lua_inner(v: &Value, depth: usize, seen: &mut HashSet<usize>) -> Result<Self> {
        if depth > MAX_DEPTH {
            return Err(LehuaError::msg("value is too deeply nested to send"));
        }
        Ok(match v {
            Value::Nil => PortableValue::Nil,
            Value::Boolean(b) => PortableValue::Bool(*b),
            Value::Integer(i) => PortableValue::Int(*i),
            Value::Number(n) => PortableValue::Num(*n),
            Value::String(s) => PortableValue::Str(s.as_bytes().to_vec()),
            Value::Table(t) => {
                let id = t.to_pointer() as usize;
                if !seen.insert(id) {
                    return Err(LehuaError::msg("cannot send a table that references itself"));
                }
                let len = t.raw_len();
                let mut entries: Vec<(PortableValue, PortableValue)> = Vec::new();
                let mut int_keys = 0usize;
                for pair in t.pairs::<Value, Value>() {
                    let (k, val) = pair?;
                    if let Value::Integer(i) = k {
                        if i >= 1 && (i as usize) <= len {
                            int_keys += 1;
                        }
                    }
                    let pk = Self::from_lua_inner(&k, depth + 1, seen)?;
                    let pv = Self::from_lua_inner(&val, depth + 1, seen)?;
                    entries.push((pk, pv));
                }
                seen.remove(&id);

                if len > 0 && entries.len() == len && int_keys == len {
                    let mut arr: Vec<PortableValue> =
                        std::iter::repeat_with(|| PortableValue::Nil).take(len).collect();
                    for (k, val) in entries {
                        if let PortableValue::Int(i) = k {
                            arr[(i - 1) as usize] = val;
                        }
                    }
                    PortableValue::Array(arr)
                } else {
                    PortableValue::Map(entries)
                }
            }
            Value::Function(_) => return Err(LehuaError::NotPortable("function")),
            Value::Thread(_) => return Err(LehuaError::NotPortable("thread")),
            #[cfg(feature = "lib-cache")]
            Value::UserData(ud) => match crate::libs::cache::memcache_name(ud) {
                Some(name) => PortableValue::MemCache(name),
                None => return Err(LehuaError::NotPortable("userdata")),
            },
            #[cfg(not(feature = "lib-cache"))]
            Value::UserData(_) => return Err(LehuaError::NotPortable("userdata")),
            Value::LightUserData(_) => return Err(LehuaError::NotPortable("lightuserdata")),
            Value::Buffer(_) => return Err(LehuaError::NotPortable("buffer")),
            Value::Vector(_) => return Err(LehuaError::NotPortable("vector")),
            Value::Error(_) => return Err(LehuaError::NotPortable("error")),
            _ => return Err(LehuaError::NotPortable("unsupported")),
        })
    }

    pub fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        Ok(match self {
            PortableValue::Nil => Value::Nil,
            PortableValue::Bool(b) => Value::Boolean(b),
            PortableValue::Int(i) => match i32::try_from(i) {
                Ok(v) => Value::Integer(v.into()),
                Err(_) => Value::Number(i as f64),
            },
            PortableValue::Num(n) => Value::Number(n),
            PortableValue::Str(bytes) => Value::String(lua.create_string(&bytes)?),
            PortableValue::Array(items) => {
                let t = lua.create_table_with_capacity(items.len(), 0)?;
                for (i, item) in items.into_iter().enumerate() {
                    t.raw_seti(i + 1, item.into_lua(lua)?)?;
                }
                Value::Table(t)
            }
            PortableValue::Map(entries) => {
                let t = lua.create_table_with_capacity(0, entries.len())?;
                for (k, v) in entries {
                    t.raw_set(k.into_lua(lua)?, v.into_lua(lua)?)?;
                }
                Value::Table(t)
            }
            #[cfg(feature = "lib-cache")]
            PortableValue::MemCache(name) => crate::libs::cache::memcache_value(lua, &name)?,
        })
    }

    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            PortableValue::Nil => J::Null,
            PortableValue::Bool(b) => J::Bool(*b),
            PortableValue::Int(i) => J::Number((*i).into()),
            PortableValue::Num(n) => serde_json::Number::from_f64(*n).map(J::Number).unwrap_or(J::Null),
            PortableValue::Str(b) => J::String(String::from_utf8_lossy(b).into_owned()),
            PortableValue::Array(items) => J::Array(items.iter().map(|v| v.to_json()).collect()),
            PortableValue::Map(entries) => {
                let mut obj = serde_json::Map::new();
                for (k, v) in entries {
                    obj.insert(k.json_key(), v.to_json());
                }
                J::Object(obj)
            }
            #[cfg(feature = "lib-cache")]
            PortableValue::MemCache(name) => J::String(format!("MemCache({name})")),
        }
    }

    fn json_key(&self) -> String {
        match self {
            PortableValue::Str(b) => String::from_utf8_lossy(b).into_owned(),
            PortableValue::Int(i) => i.to_string(),
            PortableValue::Num(n) => n.to_string(),
            PortableValue::Bool(b) => b.to_string(),
            _ => "?".to_string(),
        }
    }

    pub fn from_json(v: &serde_json::Value) -> Self {
        use serde_json::Value as J;
        match v {
            J::Null => PortableValue::Nil,
            J::Bool(b) => PortableValue::Bool(*b),
            J::Number(n) => {
                if let Some(i) = n.as_i64() {
                    PortableValue::Int(i)
                } else {
                    PortableValue::Num(n.as_f64().unwrap_or(0.0))
                }
            }
            J::String(s) => PortableValue::Str(s.as_bytes().to_vec()),
            J::Array(items) => PortableValue::Array(items.iter().map(PortableValue::from_json).collect()),
            J::Object(map) => PortableValue::Map(
                map.iter()
                    .map(|(k, v)| (PortableValue::Str(k.as_bytes().to_vec()), PortableValue::from_json(v)))
                    .collect(),
            ),
        }
    }
}
