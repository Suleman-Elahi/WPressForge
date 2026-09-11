use serde::{Deserialize, Serialize};

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors that can cross the panel <-> agent boundary.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),

    #[error("already exists: {0}")]
    Conflict(String),

    #[error("invalid request: {0}")]
    Invalid(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("operation is not supported by this agent: {0}")]
    Unsupported(String),

    /// A privileged command failed on the node.
    #[error("command `{command}` failed with status {status}: {stderr}")]
    Command {
        command: String,
        status: i32,
        stderr: String,
    },

    #[error("agent unreachable: {0}")]
    Unreachable(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    pub fn internal(msg: impl std::fmt::Display) -> Self {
        Self::Internal(msg.to_string())
    }

    pub fn invalid(msg: impl std::fmt::Display) -> Self {
        Self::Invalid(msg.to_string())
    }

    /// HTTP status the panel/agent should answer with.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::NotFound(_) => 404,
            Self::Conflict(_) => 409,
            Self::Invalid(_) => 422,
            Self::Unauthorized => 401,
            Self::Unsupported(_) => 501,
            Self::Unreachable(_) => 502,
            Self::Command { .. } | Self::Internal(_) => 500,
        }
    }
}
