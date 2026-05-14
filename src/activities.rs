//! Temporal activities backing the agent loop.
//!
//! The workflow itself stays deterministic; everything that talks to the
//! outside world — calling the LLM, executing tools — happens here. These
//! activities are stateless across invocations: all required context flows
//! in via their inputs.

use std::sync::Arc;

use autoagents_llm::LLMProvider;
use serde_json::Value;
use temporalio_macros::activities;
use temporalio_sdk::activities::{ActivityContext, ActivityError};

use crate::error::AgentError;
use crate::llm;
use crate::state::{LlmChatInput, LlmResponse, ToolCall, ToolResult};
use crate::tool::ToolRegistry;

/// Shared state for the activity worker. One instance per worker process.
///
/// `Arc<dyn LLMProvider>` and the `ToolRegistry` are both cheap to clone and
/// safe to share across concurrent activity executions.
#[derive(Clone)]
pub struct AgentActivities {
    pub llm: Arc<dyn LLMProvider>,
    pub tools: ToolRegistry,
}

impl AgentActivities {
    pub fn new(llm: Arc<dyn LLMProvider>, tools: ToolRegistry) -> Self {
        Self { llm, tools }
    }
}

#[activities]
impl AgentActivities {
    /// One LLM "reasoning step": given the running conversation and the
    /// catalog of tools, ask the model what to do next.
    ///
    /// Returns either a final answer or a list of tool calls to execute.
    #[activity]
    pub async fn llm_chat(
        self: Arc<Self>,
        _ctx: ActivityContext,
        input: LlmChatInput,
    ) -> Result<LlmResponse, ActivityError> {
        tracing::debug!(
            messages = input.messages.len(),
            tools = input.tools.len(),
            schema = input.output_schema.is_some(),
            "llm_chat: invoking LLM"
        );
        let response = llm::chat(
            &self.llm,
            &input.messages,
            &input.tools,
            input.output_schema,
        )
        .await
        .map_err(agent_err_to_activity_err)?;
        Ok(response)
    }

    /// Execute a single tool call.
    ///
    /// Tool-side failures are returned as `Ok(ToolResult { error: Some(..) })`
    /// — they are observed by the LLM, not retried by Temporal. Only
    /// infrastructure errors (missing tool registration, serde failure)
    /// surface as `Err`.
    #[activity]
    pub async fn execute_tool(
        self: Arc<Self>,
        _ctx: ActivityContext,
        call: ToolCall,
    ) -> Result<ToolResult, ActivityError> {
        let tool = self.tools.get(&call.name).ok_or_else(|| {
            agent_err_to_activity_err(AgentError::ToolNotFound(call.name.clone()))
        })?;

        tracing::debug!(name = %call.name, id = %call.id, "execute_tool: dispatching");

        match tool.execute(call.args.clone()).await {
            Ok(output) => Ok(ToolResult {
                call_id: call.id,
                output,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                call_id: call.id,
                output: Value::Null,
                error: Some(e.to_string()),
            }),
        }
    }
}

fn agent_err_to_activity_err(e: AgentError) -> ActivityError {
    // ActivityError has a blanket From<E: Into<anyhow::Error>>; AgentError
    // implements Error via thiserror so it converts cleanly.
    ActivityError::from(anyhow::Error::from(e))
}
