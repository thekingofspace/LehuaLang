use std::net::IpAddr;
use std::rc::Rc;
use std::str::FromStr;

use hickory_resolver::config::ResolverConfig;
use hickory_resolver::proto::rr::domain::Name;
use hickory_resolver::proto::rr::{RData, RecordType};
use hickory_resolver::{Resolver, TokioResolver};
use mlua::{Lua, Table, Value};

use crate::error::LehuaError;

fn make_resolver() -> mlua::Result<TokioResolver> {
    let builder = match TokioResolver::builder_tokio() {
        Ok(b) => b,
        Err(_) => Resolver::builder_with_config(
            ResolverConfig::default(),
            Default::default(),
        ),
    };
    builder.build().map_err(mlua::Error::external)
}

fn rdata_to_lua(lua: &Lua, rdata: &RData) -> mlua::Result<Value> {
    match rdata {
        RData::A(a) => Ok(Value::String(lua.create_string(a.to_string())?)),
        RData::AAAA(aaaa) => Ok(Value::String(lua.create_string(aaaa.to_string())?)),
        RData::MX(mx) => {
            let t = lua.create_table()?;
            t.set("priority", mx.preference)?;
            t.set("host", mx.exchange.to_utf8())?;
            Ok(Value::Table(t))
        }
        RData::TXT(txt) => {
            let mut joined = Vec::new();
            for chunk in txt.txt_data.iter() {
                joined.extend_from_slice(chunk);
            }
            Ok(Value::String(lua.create_string(joined)?))
        }
        RData::SRV(srv) => {
            let t = lua.create_table()?;
            t.set("priority", srv.priority)?;
            t.set("weight", srv.weight)?;
            t.set("port", srv.port)?;
            t.set("target", srv.target.to_utf8())?;
            Ok(Value::Table(t))
        }
        RData::NS(ns) => Ok(Value::String(lua.create_string(ns.0.to_utf8())?)),
        RData::PTR(ptr) => Ok(Value::String(lua.create_string(ptr.0.to_utf8())?)),
        RData::CNAME(cname) => Ok(Value::String(lua.create_string(cname.0.to_utf8())?)),
        other => Ok(Value::String(lua.create_string(other.to_string())?)),
    }
}

async fn resolve(
    lua: Lua,
    resolver: Rc<TokioResolver>,
    name: String,
    record: String,
) -> mlua::Result<Table> {
    let record = record.trim().to_ascii_uppercase();
    let rtype = RecordType::from_str(&record)
        .map_err(|_| LehuaError::msg(format!("unknown DNS record type '{record}'")))?;
    let lookup = resolver
        .lookup(name.as_str(), rtype)
        .await
        .map_err(mlua::Error::external)?;
    let out = lua.create_table()?;
    let mut i = 1usize;
    for record in lookup.answers() {
        out.raw_seti(i, rdata_to_lua(&lua, &record.data)?)?;
        i += 1;
    }
    Ok(out)
}

pub fn install(lua: &Lua, net: &Table) -> mlua::Result<()> {
    let dns = lua.create_table()?;
    let resolver = Rc::new(make_resolver()?);

    {
        let resolver = resolver.clone();
        dns.set(
            "lookup",
            lua.create_async_function(move |lua, host: String| {
                let resolver = resolver.clone();
                async move {
                    let lookup = resolver
                        .lookup_ip(host.as_str())
                        .await
                        .map_err(mlua::Error::external)?;
                    let out = lua.create_table()?;
                    for (i, ip) in (1usize..).zip(lookup.iter()) {
                        out.raw_seti(i, ip.to_string())?;
                    }
                    Ok(out)
                }
            })?,
        )?;
    }

    {
        let resolver = resolver.clone();
        dns.set(
            "lookupOne",
            lua.create_async_function(move |lua, host: String| {
                let resolver = resolver.clone();
                async move {
                    let lookup = resolver
                        .lookup_ip(host.as_str())
                        .await
                        .map_err(mlua::Error::external)?;
                    match lookup.iter().next() {
                        Some(ip) => Ok(Value::String(lua.create_string(ip.to_string())?)),
                        None => Ok(Value::Nil),
                    }
                }
            })?,
        )?;
    }

    {
        let resolver = resolver.clone();
        dns.set(
            "reverse",
            lua.create_async_function(move |lua, ip: String| {
                let resolver = resolver.clone();
                async move {
                    let addr = IpAddr::from_str(ip.trim())
                        .map_err(|_| LehuaError::msg(format!("invalid IP address '{ip}'")))?;
                    let lookup = resolver
                        .reverse_lookup(Name::from(addr))
                        .await
                        .map_err(mlua::Error::external)?;
                    let out = lua.create_table()?;
                    let mut i = 1usize;
                    for record in lookup.answers() {
                        if let RData::PTR(ptr) = &record.data {
                            out.raw_seti(i, ptr.0.to_utf8())?;
                            i += 1;
                        }
                    }
                    Ok(out)
                }
            })?,
        )?;
    }

    {
        let resolver = resolver.clone();
        dns.set(
            "resolve",
            lua.create_async_function(move |lua, (name, record): (String, String)| {
                let resolver = resolver.clone();
                async move { resolve(lua, resolver, name, record).await }
            })?,
        )?;
    }

    net.set("dns", dns)?;
    Ok(())
}
