use std::path::PathBuf;

/// Library errors. Binaries wrap these in `anyhow`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no such task: {0}")]
    TaskNotFound(String),

    #[error("no store at {0} — nothing has been created yet")]
    NoStore(PathBuf),

    #[error(
        "store at {path} has schema version {found}, but this build understands \
         at most {known} — upgrade shep"
    )]
    SchemaTooNew {
        path: PathBuf,
        found: i64,
        known: i64,
    },

    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("malformed {what} in the store: {detail}")]
    Corrupt { what: &'static str, detail: String },

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }

    pub fn corrupt(what: &'static str, detail: impl Into<String>) -> Self {
        Error::Corrupt {
            what,
            detail: detail.into(),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
