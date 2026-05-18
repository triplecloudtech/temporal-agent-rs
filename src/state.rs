//! Serializable agent state stored in workflow history.
//!
//! These types deliberately mirror — rather than re-export — AutoAgents'
//! `ChatMessage` shape so the workflow's persisted history stays decoupled
//! from upstream version churn. Conversions happen at the activity boundary
//! in [`crate::llm`].

use autoagents_llm::chat::StructuredOutputFormat;
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
    /// Optional JSON Schema constraining the model's final answer. When set,
    /// the schema is forwarded to the provider on every `llm_chat` activity
    /// call and recorded in workflow history alongside `messages` and
    /// `tools`, so replay re-issues byte-identical requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<StructuredOutputFormat>,
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
    /// Mirror of [`AgentInput::output_schema`] — copied into every activity
    /// invocation so the schema lives in event history and replay is
    /// deterministic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<StructuredOutputFormat>,
}

/// Description of a tool sent to the LLM so it knows what it can call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub args_schema: serde_json::Value,
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
            output_schema: None,
        });
        assert_eq!(s.history.len(), 2);
        assert_eq!(s.history[0].role, Role::System);
        assert_eq!(s.history[1].role, Role::User);
        assert_eq!(s.turn, 0);
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

    fn sample_schema() -> StructuredOutputFormat {
        StructuredOutputFormat {
            name: "weather_report".into(),
            description: Some("Structured weather observation".into()),
            schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" },
                    "temperature_c": { "type": "number" },
                },
                "required": ["city", "temperature_c"]
            })),
            strict: Some(true),
        }
    }

    #[test]
    fn llm_chat_input_roundtrips_with_schema() {
        // Stable serde of LlmChatInput is the load-bearing guarantee for
        // workflow replay determinism: Temporal hashes activity inputs as
        // JSON, so any drift breaks history matching.
        let input = LlmChatInput {
            messages: vec![Message::user("hello")],
            tools: vec![ToolSchema {
                name: "noop".into(),
                description: "does nothing".into(),
                args_schema: serde_json::json!({"type": "object"}),
            }],
            output_schema: Some(sample_schema()),
        };
        let s = serde_json::to_string(&input).unwrap();
        let back: LlmChatInput = serde_json::from_str(&s).unwrap();
        assert_eq!(back.messages, input.messages);
        assert_eq!(back.output_schema, input.output_schema);
    }

    #[test]
    fn llm_chat_input_omits_absent_schema() {
        // Backward-compat: an `output_schema: None` must not appear in the
        // serialized form, so existing recorded inputs deserialize cleanly.
        let input = LlmChatInput {
            messages: vec![],
            tools: vec![],
            output_schema: None,
        };
        let s = serde_json::to_string(&input).unwrap();
        assert!(!s.contains("output_schema"), "got {s}");
    }

    #[test]
    fn agent_input_deserializes_without_schema_field() {
        // Recorded AgentInput JSON from before this field existed must still
        // load — guarantees forward compatibility for in-flight workflows
        // upgraded across this release.
        let legacy = r#"{"system_prompt":"s","user_message":"u","max_turns":3}"#;
        let parsed: AgentInput = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.max_turns, 3);
        assert!(parsed.output_schema.is_none());
    }
}
