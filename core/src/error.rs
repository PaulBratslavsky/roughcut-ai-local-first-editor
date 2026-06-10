use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid argument: {0}")]
    InvalidArg(String),
    #[error("unavailable: {0}")]
    Unavailable(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("inference error: {0}")]
    Inference(String),
    #[error("export error: {0}")]
    Export(String),
    #[error("{0}")]
    Other(String),
}

impl CoreError {
    pub fn code(&self) -> &'static str {
        match self {
            CoreError::NotFound(_) => "not_found",
            CoreError::InvalidArg(_) => "invalid_argument",
            CoreError::Unavailable(_) => "unavailable",
            CoreError::Io(_) => "io",
            CoreError::Storage(_) => "storage",
            CoreError::Inference(_) => "inference",
            CoreError::Export(_) => "export",
            CoreError::Other(_) => "internal",
        }
    }

    /// JSON shape used by the MCP server and generic callers.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "error": { "code": self.code(), "message": self.to_string() } })
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(e: serde_json::Error) -> Self {
        CoreError::InvalidArg(e.to_string())
    }
}

impl From<rusqlite::Error> for CoreError {
    fn from(e: rusqlite::Error) -> Self {
        CoreError::Storage(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;
