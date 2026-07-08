use std::io::{BufRead, IsTerminal, Read, Write};

use mlua::{Value, Variadic};

use super::LibCtx;
use crate::error::LehuaError;

fn write_values(out: &mut dyn Write, args: &Variadic<Value>) -> mlua::Result<()> {
    for a in args.iter() {
        match a {
            Value::String(s) => out.write_all(&s.as_bytes()).map_err(mlua::Error::external)?,
            Value::Integer(i) => out
                .write_all(i.to_string().as_bytes())
                .map_err(mlua::Error::external)?,
            Value::Number(n) => out
                .write_all(n.to_string().as_bytes())
                .map_err(mlua::Error::external)?,
            Value::Boolean(b) => out
                .write_all(if *b { b"true" } else { b"false" })
                .map_err(mlua::Error::external)?,
            other => {
                return Err(LehuaError::msg(format!(
                    "stdio.write expects strings, numbers, or booleans, got {}",
                    other.type_name()
                ))
                .into())
            }
        }
    }
    out.flush().map_err(mlua::Error::external)?;
    Ok(())
}

fn read_line() -> mlua::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let n = std::io::stdin()
        .lock()
        .read_until(b'\n', &mut line)
        .map_err(mlua::Error::external)?;
    if n == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    Ok(Some(line))
}

const COLORS: &[(&str, &str)] = &[
    ("reset", "39"),
    ("black", "30"),
    ("red", "31"),
    ("green", "32"),
    ("yellow", "33"),
    ("blue", "34"),
    ("magenta", "35"),
    ("cyan", "36"),
    ("white", "37"),
    ("brightBlack", "90"),
    ("brightRed", "91"),
    ("brightGreen", "92"),
    ("brightYellow", "93"),
    ("brightBlue", "94"),
    ("brightMagenta", "95"),
    ("brightCyan", "96"),
    ("brightWhite", "97"),
];

const BG_COLORS: &[(&str, &str)] = &[
    ("reset", "49"),
    ("black", "40"),
    ("red", "41"),
    ("green", "42"),
    ("yellow", "43"),
    ("blue", "44"),
    ("magenta", "45"),
    ("cyan", "46"),
    ("white", "47"),
    ("brightBlack", "100"),
    ("brightRed", "101"),
    ("brightGreen", "102"),
    ("brightYellow", "103"),
    ("brightBlue", "104"),
    ("brightMagenta", "105"),
    ("brightCyan", "106"),
    ("brightWhite", "107"),
];

const STYLES: &[(&str, &str)] = &[
    ("reset", "0"),
    ("bold", "1"),
    ("dim", "2"),
    ("italic", "3"),
    ("underline", "4"),
    ("blink", "5"),
    ("reverse", "7"),
    ("strikethrough", "9"),
];

fn lookup(table: &[(&str, &str)], kind: &str, name: &str) -> mlua::Result<String> {
    for (n, code) in table {
        if *n == name {
            return Ok(format!("\u{1b}[{code}m"));
        }
    }
    let known: Vec<&str> = table.iter().map(|(n, _)| *n).collect();
    Err(LehuaError::msg(format!(
        "unknown {kind} '{name}' (supported: {})",
        known.join(", ")
    ))
    .into())
}

pub fn build(ctx: &LibCtx) -> mlua::Result<Value> {
    let lua = ctx.lua;
    let t = lua.create_table()?;

    t.set(
        "write",
        lua.create_function(|_, args: Variadic<Value>| {
            write_values(&mut std::io::stdout().lock(), &args)
        })?,
    )?;

    t.set(
        "ewrite",
        lua.create_function(|_, args: Variadic<Value>| {
            write_values(&mut std::io::stderr().lock(), &args)
        })?,
    )?;

    t.set(
        "readLine",
        lua.create_function(|lua, ()| match read_line()? {
            Some(line) => Ok(Value::String(lua.create_string(line)?)),
            None => Ok(Value::Nil),
        })?,
    )?;

    t.set(
        "readAll",
        lua.create_function(|lua, ()| {
            let mut buf = Vec::new();
            std::io::stdin()
                .lock()
                .read_to_end(&mut buf)
                .map_err(mlua::Error::external)?;
            lua.create_string(buf)
        })?,
    )?;

    t.set(
        "prompt",
        lua.create_function(
            |lua, (message, options): (Option<mlua::LuaString>, Option<Vec<String>>)| {
                let write_msg = |text: &[u8]| -> mlua::Result<()> {
                    let mut out = std::io::stdout().lock();
                    out.write_all(text).map_err(mlua::Error::external)?;
                    out.flush().map_err(mlua::Error::external)?;
                    Ok(())
                };
                let options = match options {
                    None => {
                        if let Some(msg) = &message {
                            write_msg(&msg.as_bytes())?;
                        }
                        return match read_line()? {
                            Some(line) => Ok((Value::String(lua.create_string(line)?), Value::Nil)),
                            None => Ok((Value::Nil, Value::Nil)),
                        };
                    }
                    Some(o) => o,
                };
                if options.is_empty() {
                    return Err(LehuaError::msg("stdio.prompt: options list is empty").into());
                }
                let mut menu = String::new();
                if let Some(msg) = &message {
                    menu.push_str(&msg.to_string_lossy());
                    if !menu.ends_with('\n') {
                        menu.push('\n');
                    }
                }
                for (i, opt) in options.iter().enumerate() {
                    menu.push_str(&format!("  {}) {opt}\n", i + 1));
                }
                write_msg(menu.as_bytes())?;
                loop {
                    write_msg(b"> ")?;
                    let line = match read_line()? {
                        Some(line) => String::from_utf8_lossy(&line).trim().to_string(),
                        None => return Ok((Value::Nil, Value::Nil)),
                    };
                    if let Ok(n) = line.parse::<usize>() {
                        if n >= 1 && n <= options.len() {
                            return Ok((
                                Value::String(lua.create_string(&options[n - 1])?),
                                Value::Integer(n as i64),
                            ));
                        }
                    }
                    if let Some(idx) = options
                        .iter()
                        .position(|o| o.eq_ignore_ascii_case(&line))
                    {
                        return Ok((
                            Value::String(lua.create_string(&options[idx])?),
                            Value::Integer((idx + 1) as i64),
                        ));
                    }
                    write_msg(format!("pick 1-{}\n", options.len()).as_bytes())?;
                }
            },
        )?,
    )?;

    t.set(
        "isTTY",
        lua.create_function(|_, which: Option<String>| {
            match which.as_deref().unwrap_or("stdout") {
                "stdin" => Ok(std::io::stdin().is_terminal()),
                "stdout" => Ok(std::io::stdout().is_terminal()),
                "stderr" => Ok(std::io::stderr().is_terminal()),
                other => Err(LehuaError::msg(format!(
                    "unknown stream '{other}' (supported: stdin, stdout, stderr)"
                ))
                .into()),
            }
        })?,
    )?;

    t.set(
        "color",
        lua.create_function(|_, name: String| lookup(COLORS, "color", &name))?,
    )?;

    t.set(
        "bgColor",
        lua.create_function(|_, name: String| lookup(BG_COLORS, "background color", &name))?,
    )?;

    t.set(
        "style",
        lua.create_function(|_, name: String| lookup(STYLES, "style", &name))?,
    )?;

    t.set(
        "stripColors",
        lua.create_function(|lua, text: mlua::LuaString| {
            let bytes = text.as_bytes();
            let mut out = Vec::with_capacity(bytes.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                    i += 2;
                    while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    i += 1;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            lua.create_string(out)
        })?,
    )?;

    Ok(Value::Table(t))
}
