//! # temporal-agent-rs
//!
//! Durable AI agent execution on top of [Temporal](https://temporal.io) using
//! [AutoAgents](https://github.com/liquidos-ai/AutoAgents) for the
//! LLM-provider and tool abstractions.
//!
//! ## Pattern
//!
//! The library re-implements the ReAct loop inside a Temporal workflow
//! ([`AgentWorkflow`]). The workflow stays deterministic; every LLM call and
//! every tool invocation is checkpointed as a separate Temporal activity. A
//! mid-loop crash resumes from the last completed activity without
//! re-spending LLM tokens.
//!
//! ## Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use temporal_agent_rs::prelude::*;
//!
//! # async fn run(client: temporalio_client::Client, llm: Arc<dyn LLMProvider>, tool: Arc<dyn ToolT>) -> anyhow::Result<()> {
//! let runtime = temporalio_sdk_core::CoreRuntime::new_assume_tokio(Default::default())?;
//! let mut worker = AgentWorkerBuilder::new(client)
//!     .llm(llm)
//!     .tool(tool)
//!     .queue("agents")
//!     .build_worker(&runtime)?;
//! worker.run().await?;
//! # Ok(())
//! # }
//! ```
//!
//! See the `math_agent` example for the full client-side flow.

pub mod activities;
pub mod builder;
pub mod error;
pub mod llm;
pub mod prelude;
pub mod state;
pub mod tool;
pub mod workflow;

pub use crate::activities::AgentActivities;
pub use crate::builder::AgentWorkerBuilder;
pub use crate::error::AgentError;
pub use crate::state::{
    AgentInput, AgentOutput, AgentState, LlmChatInput, LlmResponse, Message, Role, StopReason,
    ToolCall, ToolResult, ToolSchema,
};
pub use crate::tool::{ToolRegistry, ToolRegistryBuilder};
pub use crate::workflow::AgentWorkflow;

// Pipeline composition (re-exports from `autoagents_llm`).
pub use autoagents_llm::pipeline::PipelineBuilder;

// Production primitives: cache and fallback. See `prelude` for the rationale
// behind intentionally excluding the retry layer.
pub use autoagents_llm::optim::{
    CacheConfig, CacheLayer, ChatCacheKeyMode, FallbackConfig, FallbackLayer,
    default_is_fallbackable,
};
