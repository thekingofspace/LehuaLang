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

impl From<LehuaError> for mlua::Error {
    fn from(e: LehuaError) -> Self {
        match e {
            LehuaError::Lua(inner) => inner,
            other => mlua::Error::external(other),
        }
    }
}
