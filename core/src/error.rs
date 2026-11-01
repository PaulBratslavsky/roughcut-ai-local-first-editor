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
    /// A Hosted-mode request that must not be papered over with a local or
    /// rule-based result (see [`CoreError::is_fatal_model_error`]).
    #[error("{message}")]
    Hosted { kind: HostedErrorKind, message: String },
    #[error("{0}")]
    Other(String),
}

/// What went wrong with a Hosted (Claude) request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostedErrorKind {
    /// Hosted is on but no API key is stored.
    NoKey,
    /// Hosted is on but the user never confirmed what it sends.
    NotConsented,
    /// Anthropic rejected the key (401 / 403).
    KeyRejected,
    /// Anthropic rejected the request (other 4xx): a bug or a bad model id.
    BadRequest,
    /// Claude (and its fallback) declined the request.
    Refused,
    /// The reply hit `max_tokens`.
    Truncated,
}

impl HostedErrorKind {
    fn code(self) -> &'static str {
        match self {
            HostedErrorKind::NoKey => "hosted_no_key",
            HostedErrorKind::NotConsented => "hosted_not_consented",
            HostedErrorKind::KeyRejected => "hosted_key_rejected",
            HostedErrorKind::BadRequest => "hosted_bad_request",
            HostedErrorKind::Refused => "hosted_refused",
            HostedErrorKind::Truncated => "hosted_truncated",
        }
    }
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
            CoreError::Hosted { kind, .. } => kind.code(),
            CoreError::Other(_) => "internal",
        }
    }

    pub fn hosted(kind: HostedErrorKind, message: impl Into<String>) -> Self {
        CoreError::Hosted { kind, message: message.into() }
    }

    /// An account or configuration problem with Hosted mode: no key, no
    /// consent, a rejected key, a rejected request. Callers must show it and
    /// stop — never fall back to rules or offline edits, which would hide it
    /// (and, for the chat agent, edit the video under a wrong explanation).
    /// A refusal or a truncated reply is about one request's content, not the
    /// account: batch loops may skip that item as they skip a malformed reply.
    pub fn is_fatal_model_error(&self) -> bool {
        matches!(
            self,
            CoreError::Hosted {
                kind: HostedErrorKind::NoKey
                    | HostedErrorKind::NotConsented
                    | HostedErrorKind::KeyRejected
                    | HostedErrorKind::BadRequest,
                ..
            }
        )
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
