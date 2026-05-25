use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("missing environment variable {0}")]
    MissingEnv(&'static str),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("configuration directory is unavailable on this system")]
    ConfigDirUnavailable,

    #[error("token file does not exist; run `baidupan login` first")]
    NotLoggedIn,

    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("system clock error: {0}")]
    Time(String),

    #[error("API error: {0}")]
    Api(String),

    #[error("invalid remote path: {0}")]
    InvalidRemotePath(String),

    #[error("cryptography error: {0}")]
    Crypto(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        let path = path.into();
        self.map_err(|source| Error::Io { path, source })
    }
}
