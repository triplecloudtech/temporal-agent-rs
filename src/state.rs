//! Serializable agent state stored in workflow history.
//!
//! These types deliberately mirror — rather than re-export — AutoAgents'
//! `ChatMessage` shape so the workflow's persisted history stays decoupled
//! from upstream version churn. Conversions happen at the activity boundary
//! in [`crate::llm`].

use serde::{Deserialize, Serialize};

/// Initial input handed to a new `AgentWorkflow` run.
///
/// The model is configured on the [`LLMProvider`] at worker-build time, not
/// per workflow run; that's why there's no `model` field here.
///
/// [`LLMProvider`]: autoagents_llm::LLMProvider
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentInput {
    pub system_prompt: String,
    pub user_message: String,
    /// Hard cap on reasoning turns before the workflow returns.
    pub max_turns: u32,
}

/// Reason the agent stopped looping.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StopReason {
    /// The model emitted a final answer.
    FinalAnswer,
    /// Reached `max_turns` before the model finalized.
    MaxTurnsReached,
}

/// Final result of an `AgentWorkflow` run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutput {
    pub final_answer: String,
    pub stop_reason: StopReason,
    pub turns_used: u32,
    pub tool_calls: u32,
}

/// Role of a message in the conversation history.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A single tool invocation requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
}

/// Result of executing a single tool call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResult {
    pub call_id: String,
    pub output: serde_json::Value,
    /// Populated when the tool itself errored. The agent observes this and
    /// can recover; Temporal does NOT retry tool errors automatically.
    pub error: Option<String>,
}

/// A single entry in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: String,
    /// Populated on `Role::Assistant` messages that requested tool calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Populated on `Role::Tool` messages — correlates with a prior `ToolCall::id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    pub fn assistant_text(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    pub fn assistant_with_tools(calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: calls,
            tool_call_id: None,
        }
    }

    pub fn tool_result(result: &ToolResult) -> Self {
        let content = match &result.error {
            Some(err) => format!("ERROR: {err}"),
            None => result.output.to_string(),
        };
        Self {
            role: Role::Tool,
            content,
            tool_calls: vec![],
            tool_call_id: Some(result.call_id.clone()),
        }
    }
}

/// Live state of an in-flight agent run. Lives inside the workflow.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentState {
    pub input: AgentInput,
    pub history: Vec<Message>,
    pub turn: u32,
    pub tool_calls_executed: u32,
    /// Messages enqueued via the `add_user_message` signal that haven't been
    /// folded into history yet. Useful for user-initiated mid-conversation
    /// nudges; tools that need to *block* waiting on the user should
    /// implement their own answer channel rather than relying on this.
    #[serde(default)]
    pub pending_user_messages: Vec<String>,
}

impl AgentState {
    pub fn new(input: AgentInput) -> Self {
        let history = vec![
            Message::system(&input.system_prompt),
            Message::user(&input.user_message),
        ];
        Self {
            input,
            history,
            turn: 0,
            tool_calls_executed: 0,
            pending_user_messages: vec![],
        }
    }
}

/// The LLM's response on a single turn.
///
/// Human-in-the-loop intentionally does NOT have its own variant — that is
/// handled by user-registered tools whose `execute()` blocks waiting on an
/// external answer mechanism. The library treats every tool uniformly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LlmResponse {
    /// Model produced a final natural-language answer; the loop should exit.
    Final { answer: String },
    /// Model wants to invoke one or more tools before reasoning further.
    UseTools { calls: Vec<ToolCall> },
}

/// Input passed to the `llm_chat` activity each turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmChatInput {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
}

/// Description of a tool sent to the LLM so it knows what it can call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub args_schema: serde_json::Value,
}

/// Compact long histories so `continue_as_new` doesn't grow the event history
/// unbounded. Keeps the system prompt, a synthetic summary marker, and the
/// most recent `keep_recent` messages.
pub fn compact(state: &AgentState, keep_recent: usize) -> AgentInput {
    let mut summary_lines = Vec::new();
    let total = state.history.len();
    let drop_until = total.saturating_sub(keep_recent);

    for msg in state.history.iter().take(drop_until) {
        let line = match msg.role {
            Role::System if summary_lines.is_empty() => continue,
            Role::User => format!("user: {}", truncate(&msg.content, 200)),
            Role::Assistant if !msg.tool_calls.is_empty() => {
                let names: Vec<&str> = msg.tool_calls.iter().map(|c| c.name.as_str()).collect();
                format!("assistant: called tools [{}]", names.join(", "))
            }
            Role::Assistant => format!("assistant: {}", truncate(&msg.content, 200)),
            Role::Tool => format!("tool: {}", truncate(&msg.content, 120)),
            Role::System => continue,
        };
        summary_lines.push(line);
    }

    let summary = if summary_lines.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n[Prior conversation summary, {} messages dropped]\n{}",
            drop_until,
            summary_lines.join("\n")
        )
    };

    let recent_user = state
        .history
        .iter()
        .rev()
        .find(|m| m.role == Role::User)
        .map(|m| m.content.clone())
        .unwrap_or_default();

    AgentInput {
        system_prompt: format!("{}{}", state.input.system_prompt, summary),
        user_message: recent_user,
        max_turns: state.input.max_turns,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut boundary = max;
    while boundary > 0 && !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}…", &s[..boundary])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_state_seeds_system_and_user() {
        let s = AgentState::new(AgentInput {
            system_prompt: "be helpful".into(),
            user_message: "hi".into(),
            max_turns: 5,
        });
        assert_eq!(s.history.len(), 2);
        assert_eq!(s.history[0].role, Role::System);
        assert_eq!(s.history[1].role, Role::User);
        assert_eq!(s.turn, 0);
    }

    #[test]
    fn compact_keeps_system_and_recent() {
        let mut state = AgentState::new(AgentInput {
            system_prompt: "sys".into(),
            user_message: "u0".into(),
            max_turns: 50,
        });
        for i in 1..30 {
            state.history.push(Message::user(format!("u{i}")));
            state.history.push(Message::assistant_text(format!("a{i}")));
        }
        let compacted = compact(&state, 10);
        assert!(compacted.system_prompt.starts_with("sys"));
        assert!(
            compacted
                .system_prompt
                .contains("Prior conversation summary")
        );
        assert_eq!(compacted.max_turns, state.input.max_turns);
    }

    #[test]
    fn truncate_respects_utf8_char_boundary() {
        // 'é' is 2 bytes; slicing at byte 2 would split it and panic.
        let t = truncate("héllo world", 2);
        assert_eq!(t, "h…");
        // Already short — returned unchanged.
        assert_eq!(truncate("hi", 10), "hi");
        // Emoji boundary (4 bytes for 🦀).
        assert_eq!(truncate("🦀rust", 2), "…");
    }

    #[test]
    fn message_roundtrips_through_json() {
        let m = Message::assistant_with_tools(vec![ToolCall {
            id: "c1".into(),
            name: "add".into(),
            args: serde_json::json!({"a": 1, "b": 2}),
        }]);
        let s = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&s).unwrap();
        assert_eq!(m, back);
    }
}
