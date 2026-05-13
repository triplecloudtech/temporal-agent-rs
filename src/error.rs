//! Error types raised by activities and the agent loop.

use thiserror::Error;

/// Errors surfaced by Temporal activities and the agent loop.
///
/// Variants split along two axes: where the error originated (LLM, tool
/// registry, parser, serde) and whether it should be retried by Temporal
/// — see [`AgentError::is_retryable`].
#[derive(Error, Debug)]
pub enum AgentError {
    /// Upstream LLM provider error (network, rate limit, auth, etc.).
    #[error("LLM provider error: {0}")]
    Llm(String),

    /// LLM returned a tool call that the worker hasn't registered.
    #[error("tool '{0}' not registered with this worker")]
    ToolNotFound(String),

    /// LLM response couldn't be parsed into a [`crate::state::LlmResponse`].
    #[error("could not parse LLM response: {0}")]
    ResponseParse(String),

    /// JSON ser/de failure crossing the activity boundary.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Catch-all for activity-side failures we don't otherwise classify.
    #[error("{0}")]
    Other(String),
}

impl AgentError {
    /// Returns `true` when the error is worth a Temporal retry.
    ///
    /// Network blips, rate limits, and transient provider hiccups are
    /// retryable; missing tool registrations and parse failures are not —
    /// they indicate a code bug, not a transient condition.
    pub fn is_retryable(&self) -> bool {
        matches!(self, AgentError::Llm(_) | AgentError::Other(_))
    }
}
