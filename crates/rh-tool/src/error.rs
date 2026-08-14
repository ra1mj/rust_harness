//! Tool errors.

/// The concrete failure modes a tool can surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ToolErrorKind {
    /// Tool declared neither `run` nor `execute`.
    #[error("not implemented")]
    NotImplemented,
    /// Tool id not present in the registry.
    #[error("not found")]
    NotFound,
    /// Tool ran but reported a generic failure.
    #[error("execution failed")]
    Execution,
    /// A capability service the tool needs was not registered.
    #[error("missing service")]
    MissingService,
}

/// The error type every tool returns.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{kind}: {message}")]
pub struct ToolError {
    pub kind: ToolErrorKind,
    pub message: String,
}

impl ToolError {
    pub fn new(kind: ToolErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn not_implemented(id: impl Into<String>) -> Self {
        Self::new(
            ToolErrorKind::NotImplemented,
            format!("tool `{}` implements neither `run` nor `execute`", id.into()),
        )
    }

    pub fn not_found(id: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::NotFound, format!("tool `{}` not found", id.into()))
    }

    pub fn missing_service(what: impl Into<String>) -> Self {
        Self::new(
            ToolErrorKind::MissingService,
            format!("required capability service not registered: {}", what.into()),
        )
    }

    pub fn execution(message: impl Into<String>) -> Self {
        Self::new(ToolErrorKind::Execution, message)
    }
}

impl From<std::io::Error> for ToolError {
    fn from(err: std::io::Error) -> Self {
        Self::execution(err.to_string())
    }
}

impl From<serde_json::Error> for ToolError {
    fn from(err: serde_json::Error) -> Self {
        Self::execution(err.to_string())
    }
}
