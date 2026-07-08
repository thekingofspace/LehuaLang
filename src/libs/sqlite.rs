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

        m.add_method("info", |lua, this, ()| {
            this.with_conn(|conn| {
                let t = lua.create_table()?;
                let int = |sql: &str| -> mlua::Result<i64> {
                    conn.query_row(sql, [], |r| r.get(0))
                        .map_err(mlua::Error::external)
                };
                let text = |sql: &str| -> mlua::Result<String> {
                    conn.query_row(sql, [], |r| r.get(0))
                        .map_err(mlua::Error::external)
                };
                t.set("path", this.path.clone())?;
                t.set(
                    "readOnly",
                    conn.is_readonly("main")
                        .map_err(mlua::Error::external)?,
                )?;
                t.set("autocommit", conn.is_autocommit())?;
                t.set("totalChanges", conn.total_changes() as i64)?;
                t.set("lastInsertId", conn.last_insert_rowid())?;
                t.set("sqliteVersion", rusqlite::version())?;
                t.set("journalMode", text("PRAGMA journal_mode")?)?;
                t.set("encoding", text("PRAGMA encoding")?)?;
                t.set("foreignKeys", int("PRAGMA foreign_keys")? == 1)?;
                t.set("busyTimeoutMs", int("PRAGMA busy_timeout")?)?;
                t.set("pageSize", int("PRAGMA page_size")?)?;
                t.set("pageCount", int("PRAGMA page_count")?)?;
                t.set("cacheSize", int("PRAGMA cache_size")?)?;
                t.set("userVersion", int("PRAGMA user_version")?)?;
                t.set("schemaVersion", int("PRAGMA schema_version")?)?;
                Ok(t)
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

const JOURNAL_MODES: &[&str] = &["delete", "truncate", "persist", "memory", "wal", "off"];

fn open_flags(opts: &Option<Table>) -> mlua::Result<rusqlite::OpenFlags> {
    use rusqlite::OpenFlags;
    let mut read_only = false;
    let mut create = true;
    if let Some(o) = opts {
        read_only = o.get::<Option<bool>>("readOnly")?.unwrap_or(false);
        create = o.get::<Option<bool>>("create")?.unwrap_or(true);
    }
    let mut flags = OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI;
    if read_only {
        flags |= OpenFlags::SQLITE_OPEN_READ_ONLY;
    } else {
        flags |= OpenFlags::SQLITE_OPEN_READ_WRITE;
        if create {
            flags |= OpenFlags::SQLITE_OPEN_CREATE;
        }
    }
    Ok(flags)
}

fn run_pragma(conn: &Connection, name: &str, literal: &str) -> mlua::Result<()> {
    match conn.query_row(&format!("PRAGMA {name} = {literal}"), [], |_| Ok(())) {
        Ok(()) => Ok(()),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(()),
        Err(e) => Err(mlua::Error::external(e)),
    }
}

fn apply_open_opts(conn: &Connection, opts: &Option<Table>) -> mlua::Result<()> {
    let Some(o) = opts else { return Ok(()) };
    if let Some(password) = o.get::<Option<String>>("password")? {
        run_pragma(conn, "key", &format!("'{}'", password.replace('\'', "''")))?;
        let cipher: Result<String, _> =
            conn.query_row("PRAGMA cipher_version", [], |r| r.get(0));
        if cipher.is_err() {
            return Err(LehuaError::msg(
                "sqlite: this runtime was built without SQLCipher, so password protected databases are not supported",
            )
            .into());
        }
    }
    if let Some(ms) = o.get::<Option<u64>>("busyTimeoutMs")? {
        conn.busy_timeout(std::time::Duration::from_millis(ms))
            .map_err(mlua::Error::external)?;
    }
    if let Some(fk) = o.get::<Option<bool>>("foreignKeys")? {
        conn.pragma_update(None, "foreign_keys", fk)
            .map_err(mlua::Error::external)?;
    }
    if let Some(mode) = o.get::<Option<String>>("journalMode")? {
        let mode = mode.trim().to_ascii_lowercase();
        if !JOURNAL_MODES.contains(&mode.as_str()) {
            return Err(LehuaError::msg(format!(
                "sqlite: unknown journalMode '{mode}' (supported: {})",
                JOURNAL_MODES.join(", ")
            ))
            .into());
        }
        run_pragma(conn, "journal_mode", &mode)?;
    }
    if let Some(kb) = o.get::<Option<i64>>("cacheSizeKb")? {
        conn.pragma_update(None, "cache_size", -kb.max(0))
            .map_err(mlua::Error::external)?;
    }
    if let Some(pragmas) = o.get::<Option<Table>>("pragmas")? {
        for entry in pragmas.pairs::<String, Value>() {
            let (name, value) = entry?;
            if !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || name.is_empty()
            {
                return Err(
                    LehuaError::msg(format!("sqlite: invalid pragma name '{name}'")).into(),
                );
            }
            let literal = match &value {
                Value::Boolean(b) => String::from(if *b { "1" } else { "0" }),
                Value::Integer(i) => i.to_string(),
                Value::Number(n) => n.to_string(),
                Value::String(s) => {
                    let text = s.to_str()?;
                    if !text
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                        || text.is_empty()
                    {
                        return Err(LehuaError::msg(format!(
                            "sqlite: invalid value for pragma '{name}'"
                        ))
                        .into());
                    }
                    text.to_string()
                }
                other => {
                    return Err(LehuaError::msg(format!(
                        "sqlite: pragma '{name}' must be a string, number, or boolean, got {}",
                        other.type_name()
                    ))
                    .into())
                }
            };
            run_pragma(conn, &name, &literal)?;
        }
    }
    Ok(())
}

fn open(
    conn: rusqlite::Result<Connection>,
    path: String,
    opts: &Option<Table>,
) -> mlua::Result<LuaSqlite> {
    let conn = conn.map_err(|e| LehuaError::msg(format!("sqlite: could not open: {e}")))?;
    apply_open_opts(&conn, opts)?;
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
            lua.create_function(move |_, (path, opts): (String, Option<Table>)| {
                let full = scope.resolve(&path)?;
                let flags = open_flags(&opts)?;
                if flags.contains(rusqlite::OpenFlags::SQLITE_OPEN_CREATE) {
                    if let Some(parent) = full.parent() {
                        std::fs::create_dir_all(parent).map_err(mlua::Error::external)?;
                    }
                }
                open(
                    Connection::open_with_flags(&full, flags),
                    full.to_string_lossy().into_owned(),
                    &opts,
                )
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "fromConnection",
            lua.create_function(move |_, (info, opts): (Value, Option<Table>)| {
                match info {
                    Value::String(s) => {
                        let connection = s.to_str()?.to_string();
                        let flags = open_flags(&opts)?;
                        open(
                            Connection::open_with_flags(&connection, flags),
                            connection.clone(),
                            &opts,
                        )
                    }
                    Value::Table(info) => {
                        let file = info
                            .get::<Option<String>>("file")?
                            .or(info.get::<Option<String>>("path")?)
                            .unwrap_or_else(|| String::from(":memory:"));
                        let target = if file == ":memory:" || file.starts_with("file:") {
                            file
                        } else {
                            let full = scope.resolve(&file)?;
                            let create =
                                info.get::<Option<bool>>("create")?.unwrap_or(true);
                            let read_only =
                                info.get::<Option<bool>>("readOnly")?.unwrap_or(false);
                            if create && !read_only {
                                if let Some(parent) = full.parent() {
                                    std::fs::create_dir_all(parent)
                                        .map_err(mlua::Error::external)?;
                                }
                            }
                            let mut params: Vec<String> = Vec::new();
                            if let Some(cache) = info.get::<Option<String>>("cache")? {
                                let cache = cache.trim().to_ascii_lowercase();
                                if cache != "shared" && cache != "private" {
                                    return Err(LehuaError::msg(
                                        "sqlite: cache must be 'shared' or 'private'",
                                    )
                                    .into());
                                }
                                params.push(format!("cache={cache}"));
                            }
                            if info.get::<Option<bool>>("immutable")?.unwrap_or(false) {
                                params.push(String::from("immutable=1"));
                            }
                            let path = full.to_string_lossy().replace('\\', "/");
                            let mut encoded = String::with_capacity(path.len());
                            for c in path.chars() {
                                match c {
                                    '?' => encoded.push_str("%3f"),
                                    '#' => encoded.push_str("%23"),
                                    '%' => encoded.push_str("%25"),
                                    other => encoded.push(other),
                                }
                            }
                            if params.is_empty() {
                                format!("file:{encoded}")
                            } else {
                                format!("file:{encoded}?{}", params.join("&"))
                            }
                        };
                        let all_opts = Some(info);
                        let flags = open_flags(&all_opts)?;
                        open(
                            Connection::open_with_flags(&target, flags),
                            target.clone(),
                            &all_opts,
                        )
                    }
                    other => Err(LehuaError::msg(format!(
                        "sqlite.fromConnection expects a connection string or an info table, got {}",
                        other.type_name()
                    ))
                    .into()),
                }
            })?,
        )?;
    }

    t.set(
        "memory",
        lua.create_function(|_, opts: Option<Table>| {
            open(
                Connection::open_in_memory(),
                String::from(":memory:"),
                &opts,
            )
        })?,
    )?;

    Ok(Value::Table(t))
}
