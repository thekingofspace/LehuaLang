use std::cell::Cell;
use std::sync::{Arc, Mutex};

use mlua::{
    Function, IntoLua, Lua, MetaMethod, MultiValue, Table, UserData, UserDataMethods, Value,
};
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;

use super::datetime::{instant_arg, Instant};
use super::{LibCtx, PathScope};
use crate::error::LehuaError;

type SharedConn = Arc<Mutex<Option<Connection>>>;

pub(crate) struct LuaSqlite {
    conn: SharedConn,
    path: String,
    open: Cell<bool>,
    tx_lock: Arc<tokio::sync::Mutex<()>>,
}

impl LuaSqlite {
    fn new(conn: Connection, path: String) -> LuaSqlite {
        LuaSqlite {
            conn: Arc::new(Mutex::new(Some(conn))),
            path,
            open: Cell::new(true),
            tx_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) fn share_path(&self) -> Option<String> {
        if !self.open.get() {
            return None;
        }
        if self.path == ":memory:"
            || self.path.contains(":memory:")
            || self.path.contains("mode=memory")
        {
            return None;
        }
        Some(self.path.clone())
    }

    pub(crate) fn reopen(path: &str) -> mlua::Result<LuaSqlite> {
        let conn = Connection::open(path)
            .map_err(|e| LehuaError::msg(format!("sqlite: could not open: {e}")))?;
        Ok(LuaSqlite::new(conn, path.to_string()))
    }
}

async fn with_conn<T: Send + 'static>(
    conn: SharedConn,
    f: impl FnOnce(&Connection) -> mlua::Result<T> + Send + 'static,
) -> mlua::Result<T> {
    tokio::task::spawn_blocking(move || {
        let guard = conn
            .lock()
            .map_err(|_| LehuaError::msg("sqlite: connection lock poisoned"))?;
        let c = guard
            .as_ref()
            .ok_or_else(|| LehuaError::msg("this database is closed"))?;
        f(c)
    })
    .await
    .map_err(|e| mlua::Error::external(LehuaError::msg(format!("sqlite: join error: {e}"))))?
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

enum ParamSpec {
    None,
    Positional(Vec<SqlValue>),
    Named(Vec<(String, SqlValue)>),
}

fn params_spec(params: &Option<Table>) -> mlua::Result<ParamSpec> {
    let Some(t) = params else {
        return Ok(ParamSpec::None);
    };
    let len = t.raw_len();
    if len > 0 {
        let mut out = Vec::with_capacity(len);
        for i in 1..=len {
            let v: Value = t.raw_get(i)?;
            out.push(lua_to_sql(&v)?);
        }
        return Ok(ParamSpec::Positional(out));
    }
    let mut out = Vec::new();
    for entry in t.pairs::<String, Value>() {
        let (k, v) = entry?;
        out.push((k, lua_to_sql(&v)?));
    }
    Ok(ParamSpec::Named(out))
}

fn is_temporal(decl_type: Option<&str>) -> bool {
    decl_type
        .map(|d| {
            let d = d.to_ascii_uppercase();
            d.contains("DATE") || d.contains("TIME")
        })
        .unwrap_or(false)
}

fn bind_params(stmt: &mut rusqlite::Statement, params: &ParamSpec) -> mlua::Result<()> {
    match params {
        ParamSpec::None => {}
        ParamSpec::Positional(list) => {
            for (i, v) in list.iter().enumerate() {
                stmt.raw_bind_parameter(i + 1, v)
                    .map_err(mlua::Error::external)?;
            }
        }
        ParamSpec::Named(list) => {
            for (k, v) in list {
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
                stmt.raw_bind_parameter(idx, v)
                    .map_err(mlua::Error::external)?;
            }
        }
    }
    Ok(())
}

enum CellValue {
    Null,
    Int(i64),
    Real(f64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

fn cell_from_ref(value: rusqlite::types::ValueRef) -> CellValue {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => CellValue::Null,
        ValueRef::Integer(i) => CellValue::Int(i),
        ValueRef::Real(f) => CellValue::Real(f),
        ValueRef::Text(bytes) => CellValue::Text(bytes.to_vec()),
        ValueRef::Blob(bytes) => CellValue::Blob(bytes.to_vec()),
    }
}

fn sql_to_lua(lua: &Lua, value: CellValue, temporal: bool) -> mlua::Result<Value> {
    Ok(match value {
        CellValue::Null => Value::Nil,
        CellValue::Int(i) => {
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
        CellValue::Real(f) => {
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
        CellValue::Text(bytes) => {
            if temporal {
                if let Some(dt) = std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(Instant::parse_iso_like)
                {
                    return dt.into_lua(lua);
                }
            }
            Value::String(lua.create_string(&bytes)?)
        }
        CellValue::Blob(bytes) => Value::String(lua.create_string(&bytes)?),
    })
}

struct RowsOut {
    cols: Vec<(String, bool)>,
    rows: Vec<Vec<CellValue>>,
}

fn collect_rows(
    conn: &Connection,
    sql: &str,
    params: &ParamSpec,
    first_only: bool,
) -> mlua::Result<RowsOut> {
    let mut stmt = conn.prepare(sql).map_err(mlua::Error::external)?;
    bind_params(&mut stmt, params)?;
    let cols: Vec<(String, bool)> = stmt
        .columns()
        .iter()
        .map(|c| (c.name().to_string(), is_temporal(c.decl_type())))
        .collect();
    let width = cols.len();
    let mut rows = stmt.raw_query();
    let mut out: Vec<Vec<CellValue>> = Vec::new();
    while let Some(row) = rows.next().map_err(mlua::Error::external)? {
        let mut vals = Vec::with_capacity(width);
        for idx in 0..width {
            let v = row.get_ref(idx).map_err(mlua::Error::external)?;
            vals.push(cell_from_ref(v));
        }
        out.push(vals);
        if first_only {
            break;
        }
    }
    Ok(RowsOut { cols, rows: out })
}

fn row_to_table(lua: &Lua, cols: &[(String, bool)], vals: Vec<CellValue>) -> mlua::Result<Table> {
    let rt = lua.create_table()?;
    for ((name, temporal), v) in cols.iter().zip(vals) {
        rt.set(name.as_str(), sql_to_lua(lua, v, *temporal)?)?;
    }
    Ok(rt)
}

fn rows_to_lua(lua: &Lua, out: RowsOut, first_only: bool) -> mlua::Result<Value> {
    if first_only {
        match out.rows.into_iter().next() {
            Some(vals) => Ok(Value::Table(row_to_table(lua, &out.cols, vals)?)),
            None => Ok(Value::Nil),
        }
    } else {
        let table = lua.create_table()?;
        let mut i = 1usize;
        for vals in out.rows {
            table.raw_seti(i, row_to_table(lua, &out.cols, vals)?)?;
            i += 1;
        }
        Ok(Value::Table(table))
    }
}

async fn exec_simple(conn: SharedConn, sql: &'static str) -> mlua::Result<()> {
    with_conn(conn, move |c| {
        c.execute_batch(sql).map_err(mlua::Error::external)?;
        Ok(())
    })
    .await
}

struct TxRollback {
    conn: SharedConn,
    armed: bool,
}

impl Drop for TxRollback {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(guard) = self.conn.lock() {
            if let Some(c) = guard.as_ref() {
                let _ = c.execute_batch("ROLLBACK");
            }
        }
    }
}

struct DbInfo {
    read_only: bool,
    autocommit: bool,
    total_changes: i64,
    last_insert_id: i64,
    journal_mode: String,
    encoding: String,
    foreign_keys: bool,
    busy_timeout_ms: i64,
    page_size: i64,
    page_count: i64,
    cache_size: i64,
    user_version: i64,
    schema_version: i64,
}

impl UserData for LuaSqlite {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_async_method(
            "execute",
            |_, this, (sql, params): (String, Option<Table>)| {
                let conn = this.conn.clone();
                let spec = params_spec(&params);
                async move {
                    let spec = spec?;
                    with_conn(conn, move |c| {
                        let mut stmt = c.prepare(&sql).map_err(mlua::Error::external)?;
                        bind_params(&mut stmt, &spec)?;
                        let n = stmt.raw_execute().map_err(mlua::Error::external)?;
                        Ok(n)
                    })
                    .await
                }
            },
        );

        m.add_async_method("executeBatch", |_, this, sql: String| {
            let conn = this.conn.clone();
            async move {
                with_conn(conn, move |c| {
                    c.execute_batch(&sql).map_err(mlua::Error::external)?;
                    Ok(())
                })
                .await
            }
        });

        m.add_async_method("query", |lua, this, (sql, params): (String, Option<Table>)| {
            let conn = this.conn.clone();
            let spec = params_spec(&params);
            async move {
                let spec = spec?;
                let out =
                    with_conn(conn, move |c| collect_rows(c, &sql, &spec, false)).await?;
                rows_to_lua(&lua, out, false)
            }
        });

        m.add_async_method(
            "queryOne",
            |lua, this, (sql, params): (String, Option<Table>)| {
                let conn = this.conn.clone();
                let spec = params_spec(&params);
                async move {
                    let spec = spec?;
                    let out =
                        with_conn(conn, move |c| collect_rows(c, &sql, &spec, true)).await?;
                    rows_to_lua(&lua, out, true)
                }
            },
        );

        m.add_async_method("transaction", |_, this, func: Function| {
            let conn = this.conn.clone();
            let tx_lock = this.tx_lock.clone();
            async move {
                let _tx = tx_lock.lock().await;
                exec_simple(conn.clone(), "BEGIN").await?;
                let mut guard = TxRollback {
                    conn: conn.clone(),
                    armed: true,
                };
                match func.call_async::<MultiValue>(()).await {
                    Ok(values) => {
                        exec_simple(conn, "COMMIT").await?;
                        guard.armed = false;
                        Ok(values)
                    }
                    Err(e) => {
                        guard.armed = false;
                        let _ = exec_simple(conn, "ROLLBACK").await;
                        Err(e)
                    }
                }
            }
        });

        m.add_async_method("lastInsertId", |_, this, ()| {
            let conn = this.conn.clone();
            async move { with_conn(conn, |c| Ok(c.last_insert_rowid())).await }
        });

        m.add_async_method("changes", |_, this, ()| {
            let conn = this.conn.clone();
            async move { with_conn(conn, |c| Ok(c.changes() as i64)).await }
        });

        m.add_async_method("tables", |lua, this, ()| {
            let conn = this.conn.clone();
            async move {
                let names = with_conn(conn, |c| {
                    let mut stmt = c
                        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
                        .map_err(mlua::Error::external)?;
                    let mut rows = stmt.raw_query();
                    let mut out: Vec<Vec<u8>> = Vec::new();
                    while let Some(row) = rows.next().map_err(mlua::Error::external)? {
                        match row.get_ref(0).map_err(mlua::Error::external)? {
                            rusqlite::types::ValueRef::Text(bytes) => out.push(bytes.to_vec()),
                            other => out.push(format!("{other:?}").into_bytes()),
                        }
                    }
                    Ok(out)
                })
                .await?;
                let out = lua.create_table()?;
                for (i, name) in (1usize..).zip(names) {
                    out.raw_seti(i, lua.create_string(&name)?)?;
                }
                Ok(out)
            }
        });

        m.add_async_method("info", |lua, this, ()| {
            let conn = this.conn.clone();
            let path = this.path.clone();
            async move {
                let info = with_conn(conn, |c| {
                    let int = |sql: &str| -> mlua::Result<i64> {
                        c.query_row(sql, [], |r| r.get(0))
                            .map_err(mlua::Error::external)
                    };
                    let text = |sql: &str| -> mlua::Result<String> {
                        c.query_row(sql, [], |r| r.get(0))
                            .map_err(mlua::Error::external)
                    };
                    Ok(DbInfo {
                        read_only: c.is_readonly("main").map_err(mlua::Error::external)?,
                        autocommit: c.is_autocommit(),
                        total_changes: c.total_changes() as i64,
                        last_insert_id: c.last_insert_rowid(),
                        journal_mode: text("PRAGMA journal_mode")?,
                        encoding: text("PRAGMA encoding")?,
                        foreign_keys: int("PRAGMA foreign_keys")? == 1,
                        busy_timeout_ms: int("PRAGMA busy_timeout")?,
                        page_size: int("PRAGMA page_size")?,
                        page_count: int("PRAGMA page_count")?,
                        cache_size: int("PRAGMA cache_size")?,
                        user_version: int("PRAGMA user_version")?,
                        schema_version: int("PRAGMA schema_version")?,
                    })
                })
                .await?;
                let t = lua.create_table()?;
                t.set("path", path)?;
                t.set("readOnly", info.read_only)?;
                t.set("autocommit", info.autocommit)?;
                t.set("totalChanges", info.total_changes)?;
                t.set("lastInsertId", info.last_insert_id)?;
                t.set("sqliteVersion", rusqlite::version())?;
                t.set("journalMode", info.journal_mode)?;
                t.set("encoding", info.encoding)?;
                t.set("foreignKeys", info.foreign_keys)?;
                t.set("busyTimeoutMs", info.busy_timeout_ms)?;
                t.set("pageSize", info.page_size)?;
                t.set("pageCount", info.page_count)?;
                t.set("cacheSize", info.cache_size)?;
                t.set("userVersion", info.user_version)?;
                t.set("schemaVersion", info.schema_version)?;
                Ok(t)
            }
        });

        m.add_method("isOpen", |_, this, ()| Ok(this.open.get()));

        m.add_async_method("close", |_, this, ()| {
            let tx_guard = this.tx_lock.clone().try_lock_owned().map_err(|_| {
                mlua::Error::from(LehuaError::msg(
                    "sqlite: cannot close while a transaction is running",
                ))
            });
            if tx_guard.is_ok() {
                this.open.set(false);
            }
            let conn = this.conn.clone();
            async move {
                let _tx = tx_guard?;
                let taken = tokio::task::spawn_blocking(move || {
                    conn.lock()
                        .map_err(|_| {
                            mlua::Error::from(LehuaError::msg("sqlite: connection lock poisoned"))
                        })
                        .map(|mut guard| guard.take())
                })
                .await
                .map_err(|e| {
                    mlua::Error::external(LehuaError::msg(format!("sqlite: join error: {e}")))
                })??;
                if let Some(c) = taken {
                    tokio::task::spawn_blocking(move || {
                        c.close().map_err(|(_, e)| mlua::Error::external(e))
                    })
                    .await
                    .map_err(|e| {
                        mlua::Error::external(LehuaError::msg(format!("sqlite: join error: {e}")))
                    })??;
                }
                Ok(())
            }
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

struct OpenOpts {
    password: Option<String>,
    busy_timeout_ms: Option<u64>,
    foreign_keys: Option<bool>,
    journal_mode: Option<String>,
    cache_size_kb: Option<i64>,
    pragmas: Vec<(String, String)>,
}

fn open_opts(opts: &Option<Table>) -> mlua::Result<OpenOpts> {
    let mut out = OpenOpts {
        password: None,
        busy_timeout_ms: None,
        foreign_keys: None,
        journal_mode: None,
        cache_size_kb: None,
        pragmas: Vec::new(),
    };
    let Some(o) = opts else { return Ok(out) };
    out.password = o.get::<Option<String>>("password")?;
    out.busy_timeout_ms = o.get::<Option<u64>>("busyTimeoutMs")?;
    out.foreign_keys = o.get::<Option<bool>>("foreignKeys")?;
    if let Some(mode) = o.get::<Option<String>>("journalMode")? {
        let mode = mode.trim().to_ascii_lowercase();
        if !JOURNAL_MODES.contains(&mode.as_str()) {
            return Err(LehuaError::msg(format!(
                "sqlite: unknown journalMode '{mode}' (supported: {})",
                JOURNAL_MODES.join(", ")
            ))
            .into());
        }
        out.journal_mode = Some(mode);
    }
    out.cache_size_kb = o.get::<Option<i64>>("cacheSizeKb")?;
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
            out.pragmas.push((name, literal));
        }
    }
    Ok(out)
}

fn apply_open_opts(conn: &Connection, opts: &OpenOpts) -> mlua::Result<()> {
    if let Some(password) = &opts.password {
        run_pragma(conn, "key", &format!("'{}'", password.replace('\'', "''")))?;
        let cipher: Result<String, _> = conn.query_row("PRAGMA cipher_version", [], |r| r.get(0));
        if cipher.is_err() {
            return Err(LehuaError::msg(
                "sqlite: this runtime was built without SQLCipher, so password protected databases are not supported",
            )
            .into());
        }
    }
    if let Some(ms) = opts.busy_timeout_ms {
        conn.busy_timeout(std::time::Duration::from_millis(ms))
            .map_err(mlua::Error::external)?;
    }
    if let Some(fk) = opts.foreign_keys {
        conn.pragma_update(None, "foreign_keys", fk)
            .map_err(mlua::Error::external)?;
    }
    if let Some(mode) = &opts.journal_mode {
        run_pragma(conn, "journal_mode", mode)?;
    }
    if let Some(kb) = opts.cache_size_kb {
        conn.pragma_update(None, "cache_size", -kb.max(0))
            .map_err(mlua::Error::external)?;
    }
    for (name, literal) in &opts.pragmas {
        run_pragma(conn, name, literal)?;
    }
    Ok(())
}

async fn open_async(
    target: String,
    flags: rusqlite::OpenFlags,
    opts: OpenOpts,
    ensure_parent: Option<std::path::PathBuf>,
) -> mlua::Result<LuaSqlite> {
    let path = target.clone();
    let conn = tokio::task::spawn_blocking(move || -> mlua::Result<Connection> {
        if let Some(parent) = ensure_parent {
            std::fs::create_dir_all(&parent).map_err(mlua::Error::external)?;
        }
        let conn = Connection::open_with_flags(&target, flags)
            .map_err(|e| LehuaError::msg(format!("sqlite: could not open: {e}")))?;
        apply_open_opts(&conn, &opts)?;
        Ok(conn)
    })
    .await
    .map_err(|e| mlua::Error::external(LehuaError::msg(format!("sqlite: join error: {e}"))))??;
    Ok(LuaSqlite::new(conn, path))
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;
    let scope = PathScope::new(ctx);

    {
        let scope = scope.clone();
        t.set(
            "fromLocal",
            lua.create_async_function(move |_, (path, opts): (String, Option<Table>)| {
                let scope = scope.clone();
                async move {
                    let full = scope.resolve(&path)?;
                    let flags = open_flags(&opts)?;
                    let parsed = open_opts(&opts)?;
                    let parent = if flags.contains(rusqlite::OpenFlags::SQLITE_OPEN_CREATE) {
                        full.parent().map(|p| p.to_path_buf())
                    } else {
                        None
                    };
                    open_async(full.to_string_lossy().into_owned(), flags, parsed, parent).await
                }
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "fromConnection",
            lua.create_async_function(move |_, (info, opts): (Value, Option<Table>)| {
                let scope = scope.clone();
                async move {
                    match info {
                        Value::String(s) => {
                            let connection = s.to_str()?.to_string();
                            let flags = open_flags(&opts)?;
                            let parsed = open_opts(&opts)?;
                            open_async(connection, flags, parsed, None).await
                        }
                        Value::Table(info) => {
                            let file = info
                                .get::<Option<String>>("file")?
                                .or(info.get::<Option<String>>("path")?)
                                .unwrap_or_else(|| String::from(":memory:"));
                            let mut ensure_parent = None;
                            let target = if file == ":memory:" || file.starts_with("file:") {
                                file
                            } else {
                                let full = scope.resolve(&file)?;
                                let create =
                                    info.get::<Option<bool>>("create")?.unwrap_or(true);
                                let read_only =
                                    info.get::<Option<bool>>("readOnly")?.unwrap_or(false);
                                if create && !read_only {
                                    ensure_parent = full.parent().map(|p| p.to_path_buf());
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
                            let parsed = open_opts(&all_opts)?;
                            open_async(target, flags, parsed, ensure_parent).await
                        }
                        other => Err(LehuaError::msg(format!(
                            "sqlite.fromConnection expects a connection string or an info table, got {}",
                            other.type_name()
                        ))
                        .into()),
                    }
                }
            })?,
        )?;
    }

    t.set(
        "memory",
        lua.create_function(|_, opts: Option<Table>| {
            let conn = Connection::open_in_memory()
                .map_err(|e| LehuaError::msg(format!("sqlite: could not open: {e}")))?;
            let parsed = open_opts(&opts)?;
            apply_open_opts(&conn, &parsed)?;
            Ok(LuaSqlite::new(conn, String::from(":memory:")))
        })?,
    )?;

    Ok(Value::Table(t))
}
