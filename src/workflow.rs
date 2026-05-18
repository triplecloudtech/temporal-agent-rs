//! `AgentWorkflow` — the durable agent loop.
//!
//! # Determinism contract
//!
//! Every line in this module must be deterministic across replay:
//!
//! - **No wall-clock**, **no random**, **no I/O** — all of that lives in
//!   activities ([`crate::activities`]).
//! - **Never call a [`ToolT`]** directly. The workflow holds tools by name,
//!   not by reference, and dispatches them via the `execute_tool` activity.
//! - **Never call the [`LLMProvider`]** directly. Use the `llm_chat`
//!   activity.
//!
//! Re-entering the workflow function from history must produce identical
//! commands to the original run.
//!
//! [`ToolT`]: autoagents_core::tool::ToolT
//! [`LLMProvider`]: autoagents_llm::LLMProvider

use std::sync::Arc;
use std::time::Duration;

use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{
    ActivityOptions, ContinueAsNewOptions, SyncWorkflowContext, WorkflowContext,
    WorkflowContextView, WorkflowResult,
};

use crate::activities::AgentActivities;
use crate::memory::{MemoryProvider, SlidingWindowMemory};
use crate::state::{
    AgentInput, AgentOutput, AgentState, LlmChatInput, LlmResponse, Message, StopReason,
};

/// Durable AI agent workflow.
///
/// Each invocation runs a ReAct loop until the model emits a final answer or
/// `max_turns` is reached. Every LLM call and every tool call is a separate
/// Temporal activity; crashes resume from the last completed activity without
/// re-paying for prior LLM turns.
#[workflow]
#[derive(Default)]
pub struct AgentWorkflow {
    state: AgentState,
}

#[workflow_methods]
impl AgentWorkflow {
    /// Entry point. Initializes state from `input`, then loops until the LLM
    /// emits a final answer or `max_turns` is reached.
    #[run]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        input: AgentInput,
    ) -> WorkflowResult<AgentOutput> {
        ctx.state_mut(|s| {
            s.state = AgentState::new(input);
        });

        let llm_opts = ActivityOptions::with_start_to_close_timeout(Duration::from_mins(2))
            .heartbeat_timeout(Duration::from_secs(30))
            .build();
        // Tool activities use a generous default so that long-running tools —
        // notably human-in-the-loop tools that block on the user — have time
        // to complete. Override per-deployment by writing tools that
        // self-throttle / heartbeat, or fork this constant.
        let tool_opts =
            ActivityOptions::with_start_to_close_timeout(Duration::from_hours(1)).build();

        loop {
            // Drain signal-injected user messages into history.
            ctx.state_mut(|s| {
                let pending = std::mem::take(&mut s.state.pending_user_messages);
                for msg in pending {
                    s.state.history.push(Message::user(msg));
                }
            });

            let (turn, max_turns) = ctx.state(|s| (s.state.turn, s.state.input.max_turns));

            if turn >= max_turns {
                let out = ctx.state(|s| {
                    build_output(&s.state, StopReason::MaxTurnsReached, "[max turns reached]")
                });
                return Ok(out);
            }

            // Resolve the memory provider once per iteration. In production
            // this is set by `build_worker`; the `unwrap_or_else` fallback
            // mirrors `WORKER_TOOL_CATALOG` usage below so workflow-only
            // unit-test paths still work.
            let memory: Arc<dyn MemoryProvider> = crate::builder::WORKER_MEMORY
                .get()
                .cloned()
                .unwrap_or_else(|| Arc::new(SlidingWindowMemory::default()));

            if ctx.state(|s| memory.should_compact(&s.state)) {
                let next_input = ctx.state(|s| memory.compact(&s.state));
                let history_len = ctx.state(|s| s.state.history.len());
                tracing::info!(history_len, "compacting and continuing as new");
                ctx.continue_as_new(&next_input, ContinueAsNewOptions::default())?;
                unreachable!(); // continue_as_new always returns Err
            }

            let chat_input = ctx.state(|s| LlmChatInput {
                messages: s.state.history.clone(),
                tools: crate::builder::WORKER_TOOL_CATALOG
                    .get()
                    .cloned()
                    .unwrap_or_default(),
                output_schema: s.state.input.output_schema.clone(),
            });

            let response: LlmResponse = ctx
                .start_activity(AgentActivities::llm_chat, chat_input, llm_opts.clone())
                .await?;

            match response {
                LlmResponse::Final { answer } => {
                    ctx.state_mut(|s| {
                        s.state.history.push(Message::assistant_text(&answer));
                    });
                    let out =
                        ctx.state(|s| build_output(&s.state, StopReason::FinalAnswer, &answer));
                    return Ok(out);
                }
                LlmResponse::UseTools { calls } => {
                    ctx.state_mut(|s| {
                        s.state
                            .history
                            .push(Message::assistant_with_tools(calls.clone()));
                    });

                    for call in calls {
                        let result = ctx
                            .start_activity(AgentActivities::execute_tool, call, tool_opts.clone())
                            .await?;
                        ctx.state_mut(|s| {
                            s.state.history.push(Message::tool_result(&result));
                            s.state.tool_calls_executed += 1;
                        });
                    }
                    ctx.state_mut(|s| s.state.turn += 1);
                }
            }
        }
    }

    /// Inject a new user message mid-conversation.
    ///
    /// Buffered until the start of the next loop iteration so we never mutate
    /// history concurrently with an in-flight LLM activity.
    #[signal]
    pub fn add_user_message(&mut self, _ctx: &mut SyncWorkflowContext<Self>, msg: String) {
        self.state.pending_user_messages.push(msg);
    }

    /// Read the full live state.
    #[query]
    pub fn get_state(&self, _ctx: &WorkflowContextView) -> AgentState {
        self.state.clone()
    }

    /// Cheap turn counter for monitoring.
    #[query]
    pub fn turn_count(&self, _ctx: &WorkflowContextView) -> u32 {
        self.state.turn
    }
}

fn build_output(state: &AgentState, stop_reason: StopReason, answer: &str) -> AgentOutput {
    AgentOutput {
        final_answer: answer.to_string(),
        stop_reason,
        turns_used: state.turn,
        tool_calls: state.tool_calls_executed,
    }
}
