use futures_util::TryStreamExt;
use mongodb::bson::{oid::ObjectId, Bson, Document};
use mongodb::options::{
    ClientOptions, FindOneOptions, FindOptions, IndexOptions, ReplaceOptions, UpdateOptions,
};
use mongodb::{Client, Collection, Database, IndexModel};

use mlua::serde::SerializeOptions;
use mlua::{
    Lua, LuaSerdeExt, MetaMethod, Table, UserData, UserDataMethods, UserDataRef, Value,
};

use super::datetime::{instant_arg, Instant};
use super::LibCtx;
use crate::error::LehuaError;

const MAX_DEPTH: usize = 64;

fn mongo_err(e: impl std::fmt::Display) -> mlua::Error {
    LehuaError::msg(format!("mongo: {e}")).into()
}

struct LuaObjectId {
    id: ObjectId,
}

impl UserData for LuaObjectId {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("hex", |_, this, ()| Ok(this.id.to_hex()));
        m.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(this.id.to_hex()));
        m.add_meta_method(MetaMethod::Eq, |_, this, other: Value| {
            Ok(other
                .as_userdata()
                .and_then(|u| u.borrow::<LuaObjectId>().ok())
                .map(|o| o.id == this.id)
                .unwrap_or(false))
        });
    }
}

fn lua_to_bson(lua: &Lua, v: &Value, depth: usize) -> mlua::Result<Bson> {
    if depth > MAX_DEPTH {
        return Err(LehuaError::msg("mongo: value is nested too deeply or recursive").into());
    }
    Ok(match v {
        Value::Nil => Bson::Null,
        Value::LightUserData(_) => Bson::Null,
        Value::Boolean(b) => Bson::Boolean(*b),
        Value::Integer(i) => Bson::Int64(*i),
        Value::Number(n) => Bson::Double(*n),
        Value::String(s) => match s.to_str() {
            Ok(text) => Bson::String(text.to_string()),
            Err(_) => Bson::Binary(mongodb::bson::Binary {
                subtype: mongodb::bson::spec::BinarySubtype::Generic,
                bytes: s.as_bytes().to_vec(),
            }),
        },
        Value::Buffer(b) => Bson::Binary(mongodb::bson::Binary {
            subtype: mongodb::bson::spec::BinarySubtype::Generic,
            bytes: b.to_vec(),
        }),
        Value::Table(t) => {
            let is_array = t.raw_len() > 0
                || t.metatable()
                    .map(|mt| mt == lua.array_metatable())
                    .unwrap_or(false);
            if is_array {
                let mut items = Vec::with_capacity(t.raw_len());
                for item in t.sequence_values::<Value>() {
                    items.push(lua_to_bson(lua, &item?, depth + 1)?);
                }
                Bson::Array(items)
            } else {
                Bson::Document(table_to_document(lua, t, depth + 1)?)
            }
        }
        Value::UserData(ud) => {
            if let Some(dt) = instant_arg(v) {
                Bson::DateTime(mongodb::bson::DateTime::from_millis(dt.micros / 1000))
            } else if let Ok(oid) = ud.borrow::<LuaObjectId>() {
                Bson::ObjectId(oid.id)
            } else {
                return Err(LehuaError::msg(
                    "mongo: unsupported userdata value (datetime and objectId are supported)",
                )
                .into());
            }
        }
        other => {
            return Err(LehuaError::msg(format!(
                "mongo: unsupported value type {}",
                other.type_name()
            ))
            .into())
        }
    })
}

fn table_to_document(lua: &Lua, t: &Table, depth: usize) -> mlua::Result<Document> {
    let mut doc = Document::new();
    for entry in t.pairs::<Value, Value>() {
        let (k, v) = entry?;
        let key = match &k {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => n.to_string(),
            other => {
                return Err(LehuaError::msg(format!(
                    "mongo: document keys must be strings, got {}",
                    other.type_name()
                ))
                .into())
            }
        };
        doc.insert(key, lua_to_bson(lua, &v, depth)?);
    }
    Ok(doc)
}

fn opt_document(lua: &Lua, v: &Option<Table>) -> mlua::Result<Document> {
    match v {
        Some(t) => table_to_document(lua, t, 0),
        None => Ok(Document::new()),
    }
}

fn ordered_keys_document(lua: &Lua, t: &Table) -> mlua::Result<Document> {
    if t.raw_len() > 0 {
        let mut doc = Document::new();
        for pair in t.sequence_values::<Table>() {
            let pair = pair?;
            let field: String = pair.raw_get(1)?;
            let dir: Value = pair.raw_get(2)?;
            doc.insert(field, lua_to_bson(lua, &dir, 0)?);
        }
        Ok(doc)
    } else {
        table_to_document(lua, t, 0)
    }
}

fn bson_to_lua(lua: &Lua, b: Bson) -> mlua::Result<Value> {
    Ok(match b {
        Bson::Null => lua.null(),
        Bson::Boolean(v) => Value::Boolean(v),
        Bson::Int32(v) => Value::Integer(v as i64),
        Bson::Int64(v) => Value::Integer(v),
        Bson::Double(v) => Value::Number(v),
        Bson::String(v) => Value::String(lua.create_string(&v)?),
        Bson::ObjectId(id) => Value::UserData(lua.create_userdata(LuaObjectId { id })?),
        Bson::DateTime(dt) => {
            Value::UserData(lua.create_userdata(Instant::from_micros(
                dt.timestamp_millis().saturating_mul(1000),
            )?)?)
        }
        Bson::Binary(bin) => Value::String(lua.create_string(&bin.bytes)?),
        Bson::Decimal128(d) => Value::String(lua.create_string(d.to_string())?),
        Bson::Array(items) => {
            let t = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in (1usize..).zip(items) {
                t.raw_seti(i, bson_to_lua(lua, item)?)?;
            }
            t.set_metatable(Some(lua.array_metatable()))?;
            Value::Table(t)
        }
        Bson::Document(doc) => Value::Table(document_to_table(lua, doc)?),
        other => {
            let json = other.into_relaxed_extjson();
            lua.to_value_with(
                &json,
                SerializeOptions::new()
                    .serialize_none_to_null(false)
                    .serialize_unit_to_null(false),
            )?
        }
    })
}

fn document_to_table(lua: &Lua, doc: Document) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    for (k, v) in doc {
        match bson_to_lua(lua, v)? {
            Value::Nil => {}
            value => t.set(k, value)?,
        }
    }
    Ok(t)
}

fn docs_to_array(lua: &Lua, docs: Vec<Document>) -> mlua::Result<Table> {
    let out = lua.create_table_with_capacity(docs.len(), 0)?;
    for (i, doc) in (1usize..).zip(docs) {
        out.raw_seti(i, document_to_table(lua, doc)?)?;
    }
    Ok(out)
}

struct LuaMongoClient {
    client: Client,
}

struct LuaMongoDatabase {
    db: Database,
}

struct LuaMongoCollection {
    coll: Collection<Document>,
}

impl UserData for LuaMongoClient {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("database", |lua, this, name: String| {
            lua.create_userdata(LuaMongoDatabase {
                db: this.client.database(&name),
            })
        });

        m.add_method("defaultDatabase", |lua, this, ()| {
            match this.client.default_database() {
                Some(db) => Ok(Value::UserData(
                    lua.create_userdata(LuaMongoDatabase { db })?,
                )),
                None => Ok(Value::Nil),
            }
        });

        m.add_async_method("databases", |lua, this: UserDataRef<Self>, ()| async move {
            let client = this.client.clone();
            let names = client.list_database_names().await.map_err(mongo_err)?;
            let out = lua.create_table_with_capacity(names.len(), 0)?;
            for (i, n) in (1usize..).zip(names) {
                out.raw_seti(i, n)?;
            }
            Ok(out)
        });

        m.add_async_method("close", |_, this: UserDataRef<Self>, ()| async move {
            this.client.clone().shutdown().await;
            Ok(())
        });

        m.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("MongoCluster"));
    }
}

impl UserData for LuaMongoDatabase {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("collection", |lua, this, name: String| {
            lua.create_userdata(LuaMongoCollection {
                coll: this.db.collection::<Document>(&name),
            })
        });

        m.add_method("name", |_, this, ()| Ok(this.db.name().to_string()));

        m.add_async_method("collections", |lua, this: UserDataRef<Self>, ()| async move {
            let db = this.db.clone();
            let names = db.list_collection_names().await.map_err(mongo_err)?;
            let out = lua.create_table_with_capacity(names.len(), 0)?;
            for (i, n) in (1usize..).zip(names) {
                out.raw_seti(i, n)?;
            }
            Ok(out)
        });

        m.add_async_method(
            "createCollection",
            |_, this: UserDataRef<Self>, name: String| async move {
                this.db.clone().create_collection(&name).await.map_err(mongo_err)?;
                Ok(())
            },
        );

        m.add_async_method("drop", |_, this: UserDataRef<Self>, ()| async move {
            this.db.clone().drop().await.map_err(mongo_err)?;
            Ok(())
        });

        m.add_async_method(
            "runCommand",
            |lua, this: UserDataRef<Self>, command: Table| async move {
                let doc = table_to_document(&lua, &command, 0)?;
                let reply = this.db.clone().run_command(doc).await.map_err(mongo_err)?;
                document_to_table(&lua, reply)
            },
        );

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("MongoDatabase({})", this.db.name()))
        });
    }
}

impl UserData for LuaMongoCollection {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("name", |_, this, ()| Ok(this.coll.name().to_string()));

        m.add_async_method(
            "insertOne",
            |lua, this: UserDataRef<Self>, doc: Table| async move {
                let doc = table_to_document(&lua, &doc, 0)?;
                let result = this.coll.clone().insert_one(doc).await.map_err(mongo_err)?;
                bson_to_lua(&lua, result.inserted_id)
            },
        );

        m.add_async_method(
            "insertMany",
            |lua, this: UserDataRef<Self>, docs: Table| async move {
                let mut converted = Vec::with_capacity(docs.raw_len());
                for doc in docs.sequence_values::<Table>() {
                    converted.push(table_to_document(&lua, &doc?, 0)?);
                }
                let count = converted.len();
                let result = this
                    .coll
                    .clone()
                    .insert_many(converted)
                    .await
                    .map_err(mongo_err)?;
                let out = lua.create_table_with_capacity(count, 0)?;
                for i in 0..count {
                    match result.inserted_ids.get(&i) {
                        Some(id) => out.raw_seti(i + 1, bson_to_lua(&lua, id.clone())?)?,
                        None => out.raw_seti(i + 1, false)?,
                    }
                }
                Ok(out)
            },
        );

        m.add_async_method(
            "findOne",
            |lua, this: UserDataRef<Self>, (filter, opts): (Option<Table>, Option<Table>)| async move {
                let filter = opt_document(&lua, &filter)?;
                let mut builder = FindOneOptions::builder().build();
                if let Some(o) = &opts {
                    if let Some(sort) = o.get::<Option<Table>>("sort")? {
                        builder.sort = Some(ordered_keys_document(&lua, &sort)?);
                    }
                    if let Some(projection) = o.get::<Option<Table>>("projection")? {
                        builder.projection = Some(table_to_document(&lua, &projection, 0)?);
                    }
                    if let Some(skip) = o.get::<Option<u64>>("skip")? {
                        builder.skip = Some(skip);
                    }
                }
                let found = this
                    .coll
                    .clone()
                    .find_one(filter)
                    .with_options(builder)
                    .await
                    .map_err(mongo_err)?;
                match found {
                    Some(doc) => Ok(Value::Table(document_to_table(&lua, doc)?)),
                    None => Ok(Value::Nil),
                }
            },
        );

        m.add_async_method(
            "find",
            |lua, this: UserDataRef<Self>, (filter, opts): (Option<Table>, Option<Table>)| async move {
                let filter = opt_document(&lua, &filter)?;
                let mut builder = FindOptions::builder().build();
                if let Some(o) = &opts {
                    if let Some(sort) = o.get::<Option<Table>>("sort")? {
                        builder.sort = Some(ordered_keys_document(&lua, &sort)?);
                    }
                    if let Some(projection) = o.get::<Option<Table>>("projection")? {
                        builder.projection = Some(table_to_document(&lua, &projection, 0)?);
                    }
                    if let Some(limit) = o.get::<Option<i64>>("limit")? {
                        builder.limit = Some(limit);
                    }
                    if let Some(skip) = o.get::<Option<u64>>("skip")? {
                        builder.skip = Some(skip);
                    }
                    if let Some(page) = o.get::<Option<u64>>("page")? {
                        if page < 1 {
                            return Err(LehuaError::msg("mongo: page starts at 1").into());
                        }
                        let page_size = o.get::<Option<u64>>("pageSize")?.unwrap_or(20);
                        if builder.limit.is_none() {
                            builder.limit = Some(page_size as i64);
                        }
                        if builder.skip.is_none() {
                            builder.skip = Some((page - 1) * page_size);
                        }
                    }
                }
                let cursor = this
                    .coll
                    .clone()
                    .find(filter)
                    .with_options(builder)
                    .await
                    .map_err(mongo_err)?;
                let docs: Vec<Document> = cursor.try_collect().await.map_err(mongo_err)?;
                docs_to_array(&lua, docs)
            },
        );

        m.add_async_method(
            "updateOne",
            |lua,
             this: UserDataRef<Self>,
             (filter, update, opts): (Table, Table, Option<Table>)| async move {
                let filter = table_to_document(&lua, &filter, 0)?;
                let update = table_to_document(&lua, &update, 0)?;
                let mut options = UpdateOptions::builder().build();
                if let Some(o) = &opts {
                    options.upsert = o.get::<Option<bool>>("upsert")?;
                }
                let result = this
                    .coll
                    .clone()
                    .update_one(filter, update)
                    .with_options(options)
                    .await
                    .map_err(mongo_err)?;
                let out = lua.create_table()?;
                out.set("matched", result.matched_count as f64)?;
                out.set("modified", result.modified_count as f64)?;
                if let Some(id) = result.upserted_id {
                    out.set("upsertedId", bson_to_lua(&lua, id)?)?;
                }
                Ok(out)
            },
        );

        m.add_async_method(
            "updateMany",
            |lua,
             this: UserDataRef<Self>,
             (filter, update, opts): (Table, Table, Option<Table>)| async move {
                let filter = table_to_document(&lua, &filter, 0)?;
                let update = table_to_document(&lua, &update, 0)?;
                let mut options = UpdateOptions::builder().build();
                if let Some(o) = &opts {
                    options.upsert = o.get::<Option<bool>>("upsert")?;
                }
                let result = this
                    .coll
                    .clone()
                    .update_many(filter, update)
                    .with_options(options)
                    .await
                    .map_err(mongo_err)?;
                let out = lua.create_table()?;
                out.set("matched", result.matched_count as f64)?;
                out.set("modified", result.modified_count as f64)?;
                if let Some(id) = result.upserted_id {
                    out.set("upsertedId", bson_to_lua(&lua, id)?)?;
                }
                Ok(out)
            },
        );

        m.add_async_method(
            "replaceOne",
            |lua,
             this: UserDataRef<Self>,
             (filter, replacement, opts): (Table, Table, Option<Table>)| async move {
                let filter = table_to_document(&lua, &filter, 0)?;
                let replacement = table_to_document(&lua, &replacement, 0)?;
                let mut options = ReplaceOptions::builder().build();
                if let Some(o) = &opts {
                    options.upsert = o.get::<Option<bool>>("upsert")?;
                }
                let result = this
                    .coll
                    .clone()
                    .replace_one(filter, replacement)
                    .with_options(options)
                    .await
                    .map_err(mongo_err)?;
                let out = lua.create_table()?;
                out.set("matched", result.matched_count as f64)?;
                out.set("modified", result.modified_count as f64)?;
                if let Some(id) = result.upserted_id {
                    out.set("upsertedId", bson_to_lua(&lua, id)?)?;
                }
                Ok(out)
            },
        );

        m.add_async_method(
            "deleteOne",
            |lua, this: UserDataRef<Self>, filter: Table| async move {
                let filter = table_to_document(&lua, &filter, 0)?;
                let result = this.coll.clone().delete_one(filter).await.map_err(mongo_err)?;
                Ok(result.deleted_count as f64)
            },
        );

        m.add_async_method(
            "deleteMany",
            |lua, this: UserDataRef<Self>, filter: Table| async move {
                let filter = table_to_document(&lua, &filter, 0)?;
                let result = this.coll.clone().delete_many(filter).await.map_err(mongo_err)?;
                Ok(result.deleted_count as f64)
            },
        );

        m.add_async_method(
            "count",
            |lua, this: UserDataRef<Self>, filter: Option<Table>| async move {
                let filter = opt_document(&lua, &filter)?;
                let n = this
                    .coll
                    .clone()
                    .count_documents(filter)
                    .await
                    .map_err(mongo_err)?;
                Ok(n as f64)
            },
        );

        m.add_async_method(
            "distinct",
            |lua, this: UserDataRef<Self>, (field, filter): (String, Option<Table>)| async move {
                let filter = opt_document(&lua, &filter)?;
                let values = this
                    .coll
                    .clone()
                    .distinct(&field, filter)
                    .await
                    .map_err(mongo_err)?;
                let out = lua.create_table_with_capacity(values.len(), 0)?;
                for (i, v) in (1usize..).zip(values) {
                    out.raw_seti(i, bson_to_lua(&lua, v)?)?;
                }
                Ok(out)
            },
        );

        m.add_async_method(
            "aggregate",
            |lua, this: UserDataRef<Self>, pipeline: Table| async move {
                let mut stages = Vec::with_capacity(pipeline.raw_len());
                for stage in pipeline.sequence_values::<Table>() {
                    stages.push(table_to_document(&lua, &stage?, 0)?);
                }
                let cursor = this.coll.clone().aggregate(stages).await.map_err(mongo_err)?;
                let docs: Vec<Document> = cursor.try_collect().await.map_err(mongo_err)?;
                docs_to_array(&lua, docs)
            },
        );

        m.add_async_method(
            "createIndex",
            |lua, this: UserDataRef<Self>, (keys, opts): (Table, Option<Table>)| async move {
                let keys = ordered_keys_document(&lua, &keys)?;
                let mut index_opts = IndexOptions::builder().build();
                if let Some(o) = &opts {
                    index_opts.unique = o.get::<Option<bool>>("unique")?;
                    index_opts.name = o.get::<Option<String>>("name")?;
                    index_opts.sparse = o.get::<Option<bool>>("sparse")?;
                }
                let model = IndexModel::builder().keys(keys).options(index_opts).build();
                let result = this.coll.clone().create_index(model).await.map_err(mongo_err)?;
                Ok(result.index_name)
            },
        );

        m.add_async_method("indexes", |lua, this: UserDataRef<Self>, ()| async move {
            let names = this
                .coll
                .clone()
                .list_index_names()
                .await
                .map_err(mongo_err)?;
            let out = lua.create_table_with_capacity(names.len(), 0)?;
            for (i, n) in (1usize..).zip(names) {
                out.raw_seti(i, n)?;
            }
            Ok(out)
        });

        m.add_async_method(
            "dropIndex",
            |_, this: UserDataRef<Self>, name: String| async move {
                this.coll.clone().drop_index(&name).await.map_err(mongo_err)?;
                Ok(())
            },
        );

        m.add_async_method("drop", |_, this: UserDataRef<Self>, ()| async move {
            this.coll.clone().drop().await.map_err(mongo_err)?;
            Ok(())
        });

        m.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("MongoCollection({})", this.coll.name()))
        });
    }
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    t.set(
        "connect",
        lua.create_async_function(|_, (uri, opts): (String, Option<Table>)| async move {
            let mut options = ClientOptions::parse(&uri).await.map_err(mongo_err)?;
            if let Some(o) = &opts {
                if let Some(ms) = o.get::<Option<u64>>("serverSelectionTimeoutMs")? {
                    options.server_selection_timeout =
                        Some(std::time::Duration::from_millis(ms));
                }
                if let Some(ms) = o.get::<Option<u64>>("connectTimeoutMs")? {
                    options.connect_timeout = Some(std::time::Duration::from_millis(ms));
                }
                if let Some(name) = o.get::<Option<String>>("appName")? {
                    options.app_name = Some(name);
                }
            }
            let client = Client::with_options(options).map_err(mongo_err)?;
            Ok(LuaMongoClient { client })
        })?,
    )?;

    t.set(
        "objectId",
        lua.create_function(|_, hex: Option<String>| {
            let id = match hex {
                Some(h) => ObjectId::parse_str(h.trim())
                    .map_err(|e| LehuaError::msg(format!("mongo.objectId: {e}")))?,
                None => ObjectId::new(),
            };
            Ok(LuaObjectId { id })
        })?,
    )?;

    t.set(
        "isObjectId",
        lua.create_function(|_, v: Value| {
            Ok(v.as_userdata()
                .map(|u| u.borrow::<LuaObjectId>().is_ok())
                .unwrap_or(false))
        })?,
    )?;

    t.set("null", lua.null())?;

    for (name, op) in [
        ("eq", "$eq"),
        ("ne", "$ne"),
        ("gt", "$gt"),
        ("gte", "$gte"),
        ("lt", "$lt"),
        ("lte", "$lte"),
        ("size", "$size"),
    ] {
        t.set(
            name,
            lua.create_function(move |lua, value: Value| {
                let out = lua.create_table()?;
                out.set(op, value)?;
                Ok(out)
            })?,
        )?;
    }

    for (name, op) in [
        ("inList", "$in"),
        ("notInList", "$nin"),
        ("allIn", "$all"),
    ] {
        t.set(
            name,
            lua.create_function(move |lua, values: Table| {
                let arr = lua.create_table_with_capacity(values.raw_len(), 0)?;
                for (i, v) in (1usize..).zip(values.sequence_values::<Value>()) {
                    arr.raw_seti(i, v?)?;
                }
                arr.set_metatable(Some(lua.array_metatable()))?;
                let out = lua.create_table()?;
                out.set(op, arr)?;
                Ok(out)
            })?,
        )?;
    }

    for (name, op) in [("anyOf", "$or"), ("allOf", "$and"), ("noneOf", "$nor")] {
        t.set(
            name,
            lua.create_function(move |lua, filters: Table| {
                let arr = lua.create_table_with_capacity(filters.raw_len(), 0)?;
                for (i, f) in (1usize..).zip(filters.sequence_values::<Table>()) {
                    arr.raw_seti(i, f?)?;
                }
                arr.set_metatable(Some(lua.array_metatable()))?;
                let out = lua.create_table()?;
                out.set(op, arr)?;
                Ok(out)
            })?,
        )?;
    }

    t.set(
        "exists",
        lua.create_function(|lua, flag: Option<bool>| {
            let out = lua.create_table()?;
            out.set("$exists", flag.unwrap_or(true))?;
            Ok(out)
        })?,
    )?;

    t.set(
        "between",
        lua.create_function(|lua, (min, max): (Value, Value)| {
            let out = lua.create_table()?;
            out.set("$gte", min)?;
            out.set("$lte", max)?;
            Ok(out)
        })?,
    )?;

    t.set(
        "regex",
        lua.create_function(|lua, (pattern, flags): (String, Option<String>)| {
            let out = lua.create_table()?;
            out.set("$regex", pattern)?;
            if let Some(f) = flags {
                out.set("$options", f)?;
            }
            Ok(out)
        })?,
    )?;

    t.set(
        "notOf",
        lua.create_function(|lua, operator: Table| {
            let out = lua.create_table()?;
            out.set("$not", operator)?;
            Ok(out)
        })?,
    )?;

    Ok(Value::Table(t))
}
