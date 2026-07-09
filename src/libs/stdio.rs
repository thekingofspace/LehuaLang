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

enum Key {
    Up,
    Down,
    Home,
    End,
    Enter,
    Cancel,
    Digit(usize),
}

#[cfg(windows)]
mod raw_input {
    use super::Key;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, ReadConsoleInputW, SetConsoleMode, ENABLE_ECHO_INPUT,
        ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, INPUT_RECORD, KEY_EVENT, STD_INPUT_HANDLE,
    };

    pub struct RawGuard {
        handle: HANDLE,
        mode: u32,
    }

    impl RawGuard {
        pub fn new() -> std::io::Result<Self> {
            unsafe {
                let handle = GetStdHandle(STD_INPUT_HANDLE);
                let mut mode = 0u32;
                if GetConsoleMode(handle, &mut mode) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let raw = mode & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT);
                if SetConsoleMode(handle, raw) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(Self { handle, mode })
            }
        }

        pub fn read_key(&self) -> std::io::Result<Key> {
            unsafe {
                loop {
                    let mut rec: INPUT_RECORD = std::mem::zeroed();
                    let mut n = 0u32;
                    if ReadConsoleInputW(self.handle, &mut rec, 1, &mut n) == 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if n == 0 || rec.EventType as u32 != KEY_EVENT as u32 {
                        continue;
                    }
                    let key = rec.Event.KeyEvent;
                    if key.bKeyDown == 0 {
                        continue;
                    }
                    match key.wVirtualKeyCode {
                        0x26 => return Ok(Key::Up),
                        0x28 => return Ok(Key::Down),
                        0x24 => return Ok(Key::Home),
                        0x23 => return Ok(Key::End),
                        0x0d => return Ok(Key::Enter),
                        0x1b => return Ok(Key::Cancel),
                        _ => {}
                    }
                    match key.uChar.UnicodeChar {
                        0x03 | 0x04 => return Ok(Key::Cancel),
                        c @ 0x31..=0x39 => return Ok(Key::Digit((c - 0x30) as usize)),
                        0x6b | 0x4b => return Ok(Key::Up),
                        0x6a | 0x4a => return Ok(Key::Down),
                        _ => {}
                    }
                }
            }
        }
    }

    impl Drop for RawGuard {
        fn drop(&mut self) {
            unsafe {
                SetConsoleMode(self.handle, self.mode);
            }
        }
    }
}

#[cfg(unix)]
mod raw_input {
    use super::Key;
    use std::io::Read;

    pub struct RawGuard {
        orig: libc::termios,
    }

    impl RawGuard {
        pub fn new() -> std::io::Result<Self> {
            unsafe {
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &mut t) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let orig = t;
                t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
                t.c_cc[libc::VMIN] = 1;
                t.c_cc[libc::VTIME] = 0;
                if libc::tcsetattr(0, libc::TCSANOW, &t) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(Self { orig })
            }
        }

        fn read_byte(&self) -> std::io::Result<Option<u8>> {
            let mut b = [0u8; 1];
            let n = std::io::stdin().lock().read(&mut b)?;
            Ok(if n == 0 { None } else { Some(b[0]) })
        }

        fn pending(&self) -> bool {
            let mut fds = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            unsafe { libc::poll(&mut fds, 1, 25) > 0 }
        }

        pub fn read_key(&self) -> std::io::Result<Key> {
            loop {
                let b = match self.read_byte()? {
                    None => return Ok(Key::Cancel),
                    Some(b) => b,
                };
                match b {
                    b'\r' | b'\n' => return Ok(Key::Enter),
                    0x03 | 0x04 => return Ok(Key::Cancel),
                    b'k' | b'K' => return Ok(Key::Up),
                    b'j' | b'J' => return Ok(Key::Down),
                    b'1'..=b'9' => return Ok(Key::Digit((b - b'0') as usize)),
                    0x1b => {
                        if !self.pending() {
                            return Ok(Key::Cancel);
                        }
                        match self.read_byte()? {
                            Some(b'[') | Some(b'O') => {}
                            _ => continue,
                        }
                        match self.read_byte()? {
                            Some(b'A') => return Ok(Key::Up),
                            Some(b'B') => return Ok(Key::Down),
                            Some(b'H') => return Ok(Key::Home),
                            Some(b'F') => return Ok(Key::End),
                            None => return Ok(Key::Cancel),
                            _ => continue,
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    impl Drop for RawGuard {
        fn drop(&mut self) {
            unsafe {
                libc::tcsetattr(0, libc::TCSANOW, &self.orig);
            }
        }
    }
}

fn render_menu(
    out: &mut dyn Write,
    options: &[String],
    selected: usize,
    redraw: bool,
) -> std::io::Result<()> {
    if redraw {
        write!(out, "\u{1b}[{}A", options.len())?;
    }
    for (i, opt) in options.iter().enumerate() {
        out.write_all(b"\r\x1b[2K")?;
        if i == selected {
            write!(out, "\u{1b}[1;36m> {opt}\u{1b}[0m")?;
        } else {
            write!(out, "  {opt}")?;
        }
        out.write_all(b"\n")?;
    }
    out.flush()
}

fn menu_loop(
    guard: &raw_input::RawGuard,
    out: &mut dyn Write,
    options: &[String],
) -> std::io::Result<Option<usize>> {
    let mut selected = 0usize;
    render_menu(out, options, selected, false)?;
    loop {
        match guard.read_key()? {
            Key::Up => {
                selected = if selected == 0 {
                    options.len() - 1
                } else {
                    selected - 1
                }
            }
            Key::Down => selected = (selected + 1) % options.len(),
            Key::Home => selected = 0,
            Key::End => selected = options.len() - 1,
            Key::Digit(d) if d <= options.len() => selected = d - 1,
            Key::Digit(_) => continue,
            Key::Enter => return Ok(Some(selected)),
            Key::Cancel => return Ok(None),
        }
        render_menu(out, options, selected, true)?;
    }
}

fn select_menu(message: Option<&str>, options: &[String]) -> std::io::Result<Option<usize>> {
    let guard = raw_input::RawGuard::new()?;
    let mut out = std::io::stdout().lock();
    if let Some(msg) = message {
        out.write_all(msg.as_bytes())?;
        if !msg.ends_with('\n') {
            out.write_all(b"\n")?;
        }
    }
    out.write_all(b"\x1b[?25l")?;
    let result = menu_loop(&guard, &mut out, options);
    let _ = out.write_all(b"\x1b[?25h");
    let _ = out.flush();
    result
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
                let message = message.map(|m| m.to_string_lossy().to_string());
                if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                    match select_menu(message.as_deref(), &options) {
                        Ok(Some(idx)) => {
                            return Ok((
                                Value::String(lua.create_string(&options[idx])?),
                                Value::Integer((idx + 1) as i64),
                            ))
                        }
                        Ok(None) => return Ok((Value::Nil, Value::Nil)),
                        Err(_) => {}
                    }
                }
                let mut menu = String::new();
                if let Some(msg) = &message {
                    menu.push_str(msg);
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
