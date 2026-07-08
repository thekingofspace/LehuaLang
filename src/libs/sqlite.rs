use std::cell::RefCell;

use mlua::{
    Function, IntoLua, Lua, MetaMethod, MultiValue, Table, UserData, UserDataMethods, Value,
};
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;

use super::datetime::{instant_arg, Instant};
use super::{LibCtx, PathScope};
use crate::error::LehuaError;

struct LuaSqlite {
    conn: RefCell<Option<Connection>>,
    path: String,
}

impl LuaSqlite {
    fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> mlua::Result<T>) -> mlua::Result<T> {
        let guard = self.conn.borrow();
        let conn = guard
            .as_ref()
            .ok_or_else(|| LehuaError::msg("this database is closed"))?;
        f(conn)
    }
}

fn lua_to_sql(v: &Value) -> mlua::Result<SqlValue> {
    Ok(match v {
        Value::Nil => SqlValue::Null,
        Value::Boolean(b) => SqlValue::Integer(if *b { 1 } else { 0 }),
        Value::Integer(i) => SqlValue::Integer(*i),
        Value::Number(n) => SqlValue::Real(*n),
        Value::String(s) => match s.to_str() {
            Ok(text) => SqlValue::Text(text.to_string()),
            Err(_) => SqlValue::Blob(s.as_bytes().to_vec()),
        },
        Value::Buffer(b) => SqlValue::Blob(b.to_vec()),
        Value::LightUserData(_) => SqlValue::Null,
        Value::UserData(_) => match instant_arg(v) {
            Some(dt) => SqlValue::Text(dt.to_iso()),
            None => {
                return Err(LehuaError::msg(
                    "sqlite: unsupported userdata parameter (datetime objects are supported)",
                )
                .into())
            }
        },
        other => {
            return Err(LehuaError::msg(format!(
                "sqlite: unsupported parameter type {}",
                other.type_name()
            ))
            .into())
        }
    })
}

fn is_temporal(decl_type: Option<&str>) -> bool {
    decl_type
        .map(|d| {
            let d = d.to_ascii_uppercase();
            d.contains("DATE") || d.contains("TIME")
        })
        .unwrap_or(false)
}

fn bind_params(stmt: &mut rusqlite::Statement, params: &Option<Table>) -> mlua::Result<()> {
    let Some(t) = params else { return Ok(()) };
    let len = t.raw_len();
    if len > 0 {
        for i in 1..=len {
            let v: Value = t.raw_get(i)?;
            stmt.raw_bind_parameter(i, lua_to_sql(&v)?)
                .map_err(mlua::Error::external)?;
        }
        return Ok(());
    }
    for entry in t.pairs::<String, Value>() {
        let (k, v) = entry?;
        let mut idx = None;
        for prefix in [":", "@", "$"] {
            if let Some(found) = stmt
                .parameter_index(&format!("{prefix}{k}"))
                .map_err(mlua::Error::external)?
            {
                idx = Some(found);
                break;
            }
        }
        let idx = idx.ok_or_else(|| {
            LehuaError::msg(format!("sqlite: no parameter named '{k}' in this statement"))
        })?;
        stmt.raw_bind_parameter(idx, lua_to_sql(&v)?)
            .map_err(mlua::Error::external)?;
    }
    Ok(())
}

fn sql_ref_to_lua(
    lua: &Lua,
    value: rusqlite::types::ValueRef,
    temporal: bool,
) -> mlua::Result<Value> {
    use rusqlite::types::ValueRef;
    Ok(match value {
        ValueRef::Null => Value::Nil,
        ValueRef::Integer(i) => {
            if temporal {
                Instant::from_seconds(i as f64)
                    .ok()
                    .map(|dt| dt.into_lua(lua))
                    .transpose()?
                    .unwrap_or(Value::Integer(i))
            } else {
                Value::Integer(i)
            }
        }
        ValueRef::Real(f) => {
            if temporal {
                Instant::from_seconds(f)
                    .ok()
                    .map(|dt| dt.into_lua(lua))
                    .transpose()?
                    .unwrap_or(Value::Number(f))
            } else {
                Value::Number(f)
            }
        }
        ValueRef::Text(bytes) => {
            if temporal {
                if let Some(dt) = std::str::from_utf8(bytes)
                    .ok()
                    .and_then(Instant::parse_iso_like)
                {
                    return dt.into_lua(lua);
                }
            }
            Value::String(lua.create_string(bytes)?)
        }
        ValueRef::Blob(bytes) => Value::String(lua.create_string(bytes)?),
    })
}

fn run_query(
    lua: &Lua,
    conn: &Connection,
    sql: &str,
    params: &Option<Table>,
    first_only: bool,
) -> mlua::Result<Value> {
    let mut stmt = conn.prepare(sql).map_err(mlua::Error::external)?;
    bind_params(&mut stmt, params)?;
    let cols: Vec<(String, bool)> = stmt
        .columns()
        .iter()
        .map(|c| (c.name().to_string(), is_temporal(c.decl_type())))
        .collect();
    let mut rows = stmt.raw_query();
    if first_only {
        match rows.next().map_err(mlua::Error::external)? {
            Some(row) => {
                let rt = lua.create_table()?;
                for (idx, (name, temporal)) in cols.iter().enumerate() {
                    let v = row.get_ref(idx).map_err(mlua::Error::external)?;
                    rt.set(name.as_str(), sql_ref_to_lua(lua, v, *temporal)?)?;
                }
                Ok(Value::Table(rt))
            }
            None => Ok(Value::Nil),
        }
    } else {
        let out = lua.create_table()?;
        let mut i = 1usize;
        while let Some(row) = rows.next().map_err(mlua::Error::external)? {
            let rt = lua.create_table()?;
            for (idx, (name, temporal)) in cols.iter().enumerate() {
                let v = row.get_ref(idx).map_err(mlua::Error::external)?;
                rt.set(name.as_str(), sql_ref_to_lua(lua, v, *temporal)?)?;
            }
            out.raw_seti(i, rt)?;
            i += 1;
        }
        Ok(Value::Table(out))
    }
}

impl UserData for LuaSqlite {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("execute", |_, this, (sql, params): (String, Option<Table>)| {
            this.with_conn(|conn| {
                let mut stmt = conn.prepare(&sql).map_err(mlua::Error::external)?;
                bind_params(&mut stmt, &params)?;
                let n = stmt.raw_execute().map_err(mlua::Error::external)?;
                Ok(n)
            })
        });

        m.add_method("executeBatch", |_, this, sql: String| {
            this.with_conn(|conn| {
                conn.execute_batch(&sql).map_err(mlua::Error::external)?;
                Ok(())
            })
        });

        m.add_method("query", |lua, this, (sql, params): (String, Option<Table>)| {
            this.with_conn(|conn| run_query(lua, conn, &sql, &params, false))
        });

        m.add_method("queryOne", |lua, this, (sql, params): (String, Option<Table>)| {
            this.with_conn(|conn| run_query(lua, conn, &sql, &params, true))
        });

        m.add_method("transaction", |_, this, func: Function| {
            this.with_conn(|conn| {
                conn.execute_batch("BEGIN").map_err(mlua::Error::external)?;
                Ok(())
            })?;
            match func.call::<MultiValue>(()) {
                Ok(values) => {
                    this.with_conn(|conn| {
                        conn.execute_batch("COMMIT").map_err(mlua::Error::external)?;
                        Ok(())
                    })?;
                    Ok(values)
                }
                Err(e) => {
                    let _ = this.with_conn(|conn| {
                        conn.execute_batch("ROLLBACK").map_err(mlua::Error::external)?;
                        Ok(())
                    });
                    Err(e)
                }
            }
        });

        m.add_method("lastInsertId", |_, this, ()| {
            this.with_conn(|conn| Ok(conn.last_insert_rowid()))
        });

        m.add_method("changes", |_, this, ()| {
            this.with_conn(|conn| Ok(conn.changes() as i64))
        });

        m.add_method("tables", |lua, this, ()| {
            this.with_conn(|conn| {
                run_query(
                    lua,
                    conn,
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
                    &None,
                    false,
                )
                .and_then(|v| {
                    let out = lua.create_table()?;
                    if let Value::Table(rows) = v {
                        for (i, row) in (1usize..).zip(rows.sequence_values::<Table>()) {
                            out.raw_seti(i, row?.get::<Value>("name")?)?;
                        }
                    }
                    Ok(out)
                })
            })
        });

        m.add_method("isOpen", |_, this, ()| Ok(this.conn.borrow().is_some()));

        m.add_method("close", |_, this, ()| {
            if let Some(conn) = this.conn.borrow_mut().take() {
                conn.close()
                    .map_err(|(_, e)| mlua::Error::external(e))?;
            }
            Ok(())
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("SqliteDatabase({})", this.path))
        });
    }
}

fn open(conn: rusqlite::Result<Connection>, path: String) -> mlua::Result<LuaSqlite> {
    let conn = conn.map_err(|e| LehuaError::msg(format!("sqlite: could not open: {e}")))?;
    Ok(LuaSqlite {
        conn: RefCell::new(Some(conn)),
        path,
    })
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;
    let scope = PathScope::new(ctx);

    {
        let scope = scope.clone();
        t.set(
            "fromLocal",
            lua.create_function(move |_, path: String| {
                let full = scope.resolve(&path)?;
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent).map_err(mlua::Error::external)?;
                }
                open(
                    Connection::open(&full),
                    full.to_string_lossy().into_owned(),
                )
            })?,
        )?;
    }

    t.set(
        "fromConnection",
        lua.create_function(|_, connection: String| {
            open(Connection::open(&connection), connection.clone())
        })?,
    )?;

    t.set(
        "memory",
        lua.create_function(|_, ()| {
            open(Connection::open_in_memory(), String::from(":memory:"))
        })?,
    )?;

    Ok(Value::Table(t))
}
