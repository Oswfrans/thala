//! Top-level error type for the Thala orchestration kernel.

use crate::core::state::StateError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ThalaError {
    /// A state machine transition was attempted but rejected as illegal.
    #[error("State error: {0}")]
    State(#[from] StateError),

    /// The Beads task source or sink returned an error.
    #[error("Beads error: {0}")]
    Beads(String),

    /// An execution backend (local/Modal/Cloudflare) returned an error.
    #[error("Backend error ({backend}): {message}")]
    Backend { backend: String, message: String },

    /// A human interaction channel (Slack/Discord) returned an error.
    #[error("Interaction error: {0}")]
    Interaction(String),

    /// A validator (review AI, CI checks) returned an error.
    #[error("Validation error: {0}")]
    Validation(String),

    /// The repo provider (git operations, PR creation) returned an error.
    #[error("Repo error: {0}")]
    Repo(String),

    /// The state store (SQLite) returned an error.
    #[error("Storage error: {0}")]
    Storage(String),

    /// The workflow configuration is invalid or could not be loaded.
    #[error("Workflow config error: {0}")]
    WorkflowConfig(String),

    /// An expected task was not found in the state store.
    #[error("Task not found: {0}")]
    TaskNotFound(String),

    /// An expected run was not found in the state store.
    #[error("Run not found: {0}")]
    RunNotFound(String),

    /// Catch-all for errors that don't fit the above categories.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl ThalaError {
    pub fn backend(backend: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Backend {
            backend: backend.into(),
            message: message.into(),
        }
    }

    pub fn beads(msg: impl Into<String>) -> Self {
        Self::Beads(msg.into())
    }

    pub fn storage(msg: impl Into<String>) -> Self {
        Self::Storage(msg.into())
    }

    pub fn repo(msg: impl Into<String>) -> Self {
        Self::Repo(msg.into())
    }

    pub fn interaction(msg: impl Into<String>) -> Self {
        Self::Interaction(msg.into())
    }
}
