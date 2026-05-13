//! Glob-importable prelude.
//!
//! Re-exports the types most users touch when wiring an agent worker, plus a
//! handful of AutoAgents traits ([`ToolT`], [`LLMProvider`], [`ToolRuntime`],
//! [`ToolCallError`]) that show up in user code via trait bounds and tool
//! `execute` signatures. These re-exports are intentional public surface — a
//! breaking change to them is a breaking change to this crate.
//!
//! ```ignore
//! use temporal_agent_rs::prelude::*;
//! ```

pub use crate::activities::AgentActivities;
pub use crate::builder::AgentWorkerBuilder;
pub use crate::error::AgentError;
pub use crate::state::{
    AgentInput, AgentOutput, AgentState, LlmResponse, Message, Role, StopReason, ToolCall,
    ToolResult, ToolSchema,
};
pub use crate::tool::{ToolRegistry, ToolRegistryBuilder};
pub use crate::workflow::AgentWorkflow;

pub use autoagents_core::tool::{ToolCallError, ToolRuntime, ToolT};
pub use autoagents_llm::LLMProvider;
