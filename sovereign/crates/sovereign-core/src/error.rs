// SPDX-License-Identifier: AGPL-3.0-or-later
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Inference error: {0}")]
    Inference(String),

    #[error("Model not loaded: {0}")]
    ModelNotLoaded(String),

    #[error("Routing error: {0}")]
    Routing(String),

    #[error("Planning error: {0}")]
    Planning(String),

    #[error("Execution error: {0}")]
    Execution(String),

    #[error("Tool error: {tool_id}: {message}")]
    Tool { tool_id: String, message: String },

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Not implemented: {0}")]
    NotImplemented(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Task cancelled")]
    Cancelled,

    #[error("Repair not supported for this issue type")]
    RepairNotSupported,

    #[error("Corpus update not authorised (auto_update = false in recipe)")]
    UpdateNotAuthorised,

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Serialization(e.to_string())
    }
}
