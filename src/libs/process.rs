use std::process::Command;

use mlua::{Function, Lua, Table, Value};
use sysinfo::System;

use super::{LibCtx, PathScope};
use crate::error::LehuaError;

fn parse_env_value(raw: &str) -> String {
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        let inner = &raw[1..raw.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some(other) => {
                        out.push('\\');
                        out.push(other);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        return out;
    }
    if raw.len() >= 2 && raw.starts_with('\'') && raw.ends_with('\'') {
        return raw[1..raw.len() - 1].to_string();
    }
    match raw.find(" #") {
        Some(i) => raw[..i].trim_end().to_string(),
        None => raw.to_string(),
    }
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;
    let scope = PathScope::new(ctx);

    let args_t = lua.create_table()?;
    for (i, a) in (1usize..).zip(ctx.engine.args.iter()) {
        args_t.raw_seti(i, a.clone())?;
    }
    t.set("args", args_t)?;

    t.set("pid", std::process::id())?;
    t.set("platform", std::env::consts::OS)?;
    t.set("arch", std::env::consts::ARCH)?;

    t.set(
        "exepath",
        lua.create_function(|_, ()| {
            std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(mlua::Error::external)
        })?,
    )?;

    t.set(
        "cwd",
        lua.create_function(|_, ()| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .map_err(mlua::Error::external)
        })?,
    )?;

    {
        let scope = scope.clone();
        t.set(
            "chdir",
            lua.create_function(move |_, p: String| {
                let full = scope.resolve(&p)?;
                std::env::set_current_dir(&full).map_err(mlua::Error::external)?;
                Ok(())
            })?,
        )?;
    }

    {
        let sched = ctx.sched.clone();
        t.set(
            "exit",
            lua.create_function(move |_, code: Option<i32>| -> mlua::Result<()> {
                let code = code.unwrap_or(0);
                sched.exit_code.set(code);
                sched.run_close();
                std::process::exit(code);
            })?,
        )?;
    }

    {
        let sched = ctx.sched.clone();
        t.set(
            "BindToHeartbeat",
            lua.create_function(move |_, (name, func): (String, Function)| {
                sched.bind_heartbeat(name, func);
                Ok(())
            })?,
        )?;
    }

    {
        let sched = ctx.sched.clone();
        t.set(
            "UnbindFromHeartbeat",
            lua.create_function(move |_, name: String| Ok(sched.unbind_heartbeat(&name)))?,
        )?;
    }

    {
        let sched = ctx.sched.clone();
        t.set(
            "BindToClose",
            lua.create_function(move |_, func: Function| {
                sched.close.borrow_mut().push(func);
                Ok(())
            })?,
        )?;
    }

    t.set(
        "getenv",
        lua.create_function(|_, name: String| Ok(std::env::var(&name).ok()))?,
    )?;

    t.set(
        "setenv",
        lua.create_function(|_, (name, value): (String, Option<String>)| {
            match value {
                Some(v) => std::env::set_var(&name, v),
                None => std::env::remove_var(&name),
            }
            Ok(())
        })?,
    )?;

    t.set(
        "env",
        lua.create_function(|lua, ()| {
            let out = lua.create_table()?;
            for (k, v) in std::env::vars() {
                out.raw_set(k, v)?;
            }
            Ok(out)
        })?,
    )?;

    {
        let scope = scope.clone();
        t.set(
            "loadEnv",
            lua.create_function(move |lua, (path, opts): (Option<String>, Option<Table>)| {
                let full = scope.resolve(path.as_deref().unwrap_or(".env"))?;
                let text = std::fs::read_to_string(&full).map_err(|e| {
                    LehuaError::msg(format!("could not read env file '{}': {e}", full.display()))
                })?;
                let mut apply = true;
                let mut overwrite = false;
                if let Some(o) = &opts {
                    if let Some(a) = o.get::<Option<bool>>("apply")? {
                        apply = a;
                    }
                    if let Some(ov) = o.get::<Option<bool>>("override")? {
                        overwrite = ov;
                    }
                }
                let out = lua.create_table()?;
                for raw in text.lines() {
                    let line = raw.trim();
                    if line.is_empty() || line.starts_with('#') {
                        continue;
                    }
                    let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
                    let Some(eq) = line.find('=') else { continue };
                    let key = line[..eq].trim();
                    if key.is_empty()
                        || !key
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                    {
                        continue;
                    }
                    let raw_value = line[eq + 1..].trim();
                    let value = parse_env_value(raw_value);
                    if apply && (overwrite || std::env::var_os(key).is_none()) {
                        std::env::set_var(key, &value);
                    }
                    out.raw_set(key, value)?;
                }
                Ok(out)
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "run",
            lua.create_async_function(move |lua, (program, args, opts): (String, Option<Vec<String>>, Option<Table>)| {
                let scope = scope.clone();
                async move {
                    let program = if program.starts_with("./")
                        || program.starts_with("../")
                        || program.starts_with('@')
                    {
                        scope.resolve(&program)?.to_string_lossy().into_owned()
                    } else {
                        program
                    };
                    let cwd = resolve_cwd(&scope, &opts)?;
                    let output = tokio::task::spawn_blocking(move || {
                        let mut cmd = Command::new(&program);
                        cmd.args(args.unwrap_or_default());
                        if let Some(dir) = cwd {
                            cmd.current_dir(dir);
                        }
                        cmd.output()
                    })
                    .await
                    .map_err(|e| mlua::Error::external(LehuaError::msg(format!("process.run join error: {e}"))))?
                    .map_err(mlua::Error::external)?;
                    output_table(&lua, output)
                }
            })?,
        )?;
    }

    {
        let scope = scope.clone();
        t.set(
            "exec",
            lua.create_async_function(move |lua, (cmdline, opts): (String, Option<Table>)| {
                let scope = scope.clone();
                async move {
                    let cwd = resolve_cwd(&scope, &opts)?;
                    let output = tokio::task::spawn_blocking(move || {
                        let mut cmd = if cfg!(windows) {
                            let mut c = Command::new("cmd");
                            c.arg("/C").arg(&cmdline);
                            c
                        } else {
                            let mut c = Command::new("sh");
                            c.arg("-c").arg(&cmdline);
                            c
                        };
                        if let Some(dir) = cwd {
                            cmd.current_dir(dir);
                        }
                        cmd.output()
                    })
                    .await
                    .map_err(|e| mlua::Error::external(LehuaError::msg(format!("process.exec join error: {e}"))))?
                    .map_err(mlua::Error::external)?;
                    output_table(&lua, output)
                }
            })?,
        )?;
    }

    t.set(
        "sleep",
        lua.create_async_function(|_, ms: f64| async move {
            tokio::time::sleep(std::time::Duration::from_secs_f64((ms / 1000.0).max(0.0))).await;
            Ok(())
        })?,
    )?;

    t.set(
        "memory",
        lua.create_function(|lua, ()| {
            let mut sys = System::new();
            sys.refresh_memory();
            let pid = sysinfo::Pid::from_u32(std::process::id());
            sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
            let out = lua.create_table()?;
            out.set("total", sys.total_memory() as f64)?;
            out.set("available", sys.available_memory() as f64)?;
            out.set("used", sys.used_memory() as f64)?;
            if let Some(p) = sys.process(pid) {
                out.set("processUsed", p.memory() as f64)?;
            }
            Ok(out)
        })?,
    )?;

    t.set(
        "cpus",
        lua.create_function(|_, ()| {
            Ok(std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1))
        })?,
    )?;

    t.set(
        "system",
        lua.create_function(|lua, ()| {
            let out = lua.create_table()?;
            out.set("os", std::env::consts::OS)?;
            out.set("arch", std::env::consts::ARCH)?;
            out.set("family", std::env::consts::FAMILY)?;
            out.set("name", System::name().unwrap_or_default())?;
            out.set("kernel", System::kernel_version().unwrap_or_default())?;
            out.set("version", System::long_os_version().unwrap_or_default())?;
            out.set("hostname", System::host_name().unwrap_or_default())?;
            Ok(out)
        })?,
    )?;

    Ok(Value::Table(t))
}

fn resolve_cwd(scope: &PathScope, opts: &Option<Table>) -> mlua::Result<Option<std::path::PathBuf>> {
    if let Some(o) = opts {
        if let Ok(Some(dir)) = o.get::<Option<String>>("cwd") {
            return Ok(Some(scope.resolve(&dir)?));
        }
    }
    Ok(None)
}

fn output_table(lua: &Lua, output: std::process::Output) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("ok", output.status.success())?;
    t.set("code", output.status.code().unwrap_or(-1))?;
    t.set("stdout", lua.create_string(&output.stdout)?)?;
    t.set("stderr", lua.create_string(&output.stderr)?)?;
    Ok(t)
}
