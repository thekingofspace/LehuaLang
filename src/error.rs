use thiserror::Error;

pub type Result<T> = std::result::Result<T, LehuaError>;

#[derive(Debug, Error)]
pub enum LehuaError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Lua(#[from] mlua::Error),

    #[error("failed to parse TOML: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("failed to parse JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("could not resolve module '{name}' (required from '{from}')")]
    ModuleNotFound { name: String, from: String },

    #[error("root package '{0}' not found under '{1}'")]
    RootNotFound(String, String),

    #[error("circular require detected while loading '{0}'")]
    CircularRequire(String),

    #[error("a value of type '{0}' cannot cross a parallel/Port boundary")]
    NotPortable(&'static str),

    #[error("native library '{lib}': {message}")]
    Dll { lib: String, message: String },

    #[error("{0}")]
    Msg(String),
}

impl LehuaError {
    pub fn msg(s: impl Into<String>) -> Self {
        LehuaError::Msg(s.into())
    }
}

pub fn pretty(err: impl std::fmt::Display) -> String {
    use std::fmt::Write as _;
    use std::io::IsTerminal;

    let raw = err.to_string();
    let raw = raw.trim();
    let raw = raw.strip_prefix("runtime error: ").unwrap_or(raw);
    let (message, trace) = match raw.find("\nstack traceback:") {
        Some(i) => (&raw[..i], Some(&raw[i..])),
        None => (raw, None),
    };
    let mut out = clean_locations(message.trim_end());
    if let Some(trace) = trace {
        let mut frames = Vec::new();
        for line in trace.lines().skip(1) {
            let line = line.trim();
            if line.is_empty()
                || line.contains("__mlua_async_poll")
                || line == "[C]: in ?"
                || line == "stack traceback:"
            {
                continue;
            }
            frames.push(clean_locations(line));
        }
        if !frames.is_empty() {
            let color = std::io::stderr().is_terminal();
            let (dim, reset) = if color { ("\x1b[2m", "\x1b[0m") } else { ("", "") };
            let _ = write!(out, "{dim}");
            for f in frames {
                let _ = write!(out, "\n    at {f}");
            }
            let _ = write!(out, "{reset}");
        }
    }
    out
}

fn clean_locations(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("[string \"") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 9..];
        match after.find("\"]") {
            Some(j) => {
                out.push_str(&after[..j]);
                rest = &after[j + 2..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

impl From<LehuaError> for mlua::Error {
    fn from(e: LehuaError) -> Self {
        match e {
            LehuaError::Lua(inner) => inner,
            other => mlua::Error::external(other),
        }
    }
}
