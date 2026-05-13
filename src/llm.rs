//! Conversions between local [`Message`]s and AutoAgents' [`ChatMessage`],
//! plus the parser that turns an LLM response into an [`LlmResponse`].
//!
//! This module is the only place that touches `autoagents_llm` types in the
//! conversation hot-path. Keeping the dependency surface narrow lets us swap
//! AutoAgents versions (or replace it entirely) without churning the workflow.

use std::sync::Arc;

use autoagents_llm::LLMProvider;
use autoagents_llm::chat::{ChatMessage, ChatRole, FunctionTool, MessageType, Tool as LlmTool};
use serde_json::Value;

use crate::error::AgentError;
use crate::state::{LlmResponse, Message, Role, ToolCall, ToolSchema};

/// Convert local messages to the shape AutoAgents' providers expect.
pub fn to_autoagents_messages(messages: &[Message]) -> Vec<ChatMessage> {
    messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => ChatRole::System,
                Role::User => ChatRole::User,
                Role::Assistant => ChatRole::Assistant,
                Role::Tool => ChatRole::Tool,
            };
            ChatMessage {
                role,
                message_type: MessageType::default(),
                content: render_content(m),
            }
        })
        .collect()
}

fn render_content(m: &Message) -> String {
    if m.tool_calls.is_empty() {
        m.content.clone()
    } else {
        // Encode tool calls inside the content as a fenced JSON block so
        // providers without native tool-call support still see them. Providers
        // with native tool-call support also receive them via the `tools`
        // argument on `chat_with_tools`.
        let calls_json = serde_json::to_string(&m.tool_calls).unwrap_or_else(|_| "[]".into());
        format!("{}\n```tool_calls\n{calls_json}\n```", m.content)
    }
}

/// Convert our tool schemas into AutoAgents' native `Tool` shape so providers
/// like OpenAI can advertise them in the `tools` API field.
pub fn to_autoagents_tools(schemas: &[ToolSchema]) -> Vec<LlmTool> {
    schemas
        .iter()
        .map(|s| LlmTool {
            tool_type: "function".to_string(),
            function: FunctionTool {
                name: s.name.clone(),
                description: s.description.clone(),
                parameters: s.args_schema.clone(),
            },
        })
        .collect()
}

/// Call the LLM and parse the result into a response.
///
/// This is the only async LLM entry point inside the activity. It is invoked
/// once per agent turn. The model identifier lives on the `LLMProvider`
/// itself, configured at worker-build time.
pub async fn chat(
    llm: &Arc<dyn LLMProvider>,
    messages: &[Message],
    tools: &[ToolSchema],
) -> Result<LlmResponse, AgentError> {
    let chat_msgs = to_autoagents_messages(messages);
    let llm_tools = to_autoagents_tools(tools);

    // Pass the tool catalog so OpenAI-style providers can emit native tool
    // calls. Non-tool-aware providers ignore the `tools` argument.
    let response = llm
        .chat_with_tools(&chat_msgs, Some(&llm_tools), None)
        .await
        .map_err(|e| AgentError::Llm(e.to_string()))?;

    tracing::debug!("LLM response received with {:?}", response);

    if let Some(thought) = response.thinking() {
        tracing::info!("Agent thought: {}", thought);
    }

    // 1. Native tool-call API (OpenAI, Anthropic with their respective backends).
    if let Some(native_calls) = response.tool_calls()
        && !native_calls.is_empty()
    {
        let mut calls = Vec::with_capacity(native_calls.len());
        for tc in native_calls {
            let args: Value = if tc.function.arguments.trim().is_empty() {
                Value::Object(serde_json::Map::default())
            } else {
                serde_json::from_str(&tc.function.arguments).map_err(|e| {
                    AgentError::ResponseParse(format!(
                        "tool '{}' arguments not valid JSON: {e}",
                        tc.function.name
                    ))
                })?
            };
            calls.push(ToolCall {
                id: tc.id,
                name: tc.function.name,
                args,
            });
        }
        return Ok(LlmResponse::UseTools { calls });
    }

    // 2. Fenced-JSON fallback (for providers without native tool calls, or
    //    when a tool-aware model chose to embed calls in prose anyway).
    let text = response.text().unwrap_or_default();
    parse_response(&text)
}

/// Parse an assistant text response into either tool calls or a final answer.
///
/// Tool calls are detected via a fenced JSON block tagged `tool_calls`:
///
/// ````text
/// I'll add those for you.
/// ```tool_calls
/// [{"id": "1", "name": "add", "args": {"a": 1, "b": 2}}]
/// ```
/// ````
///
/// Anything outside the fence — or the whole response if no fence is found —
/// is taken as the final answer.
pub fn parse_response(text: &str) -> Result<LlmResponse, AgentError> {
    if let Some((before_fence, fence_body)) = extract_fence(text, "tool_calls") {
        let calls: Vec<ToolCall> = serde_json::from_str(fence_body)
            .map_err(|e| AgentError::ResponseParse(format!("tool_calls JSON: {e}")))?;
        if !calls.is_empty() {
            let _ = before_fence; // preamble is preserved in the assistant message
            return Ok(LlmResponse::UseTools { calls });
        }
    }

    let answer = text.trim().to_string();
    if answer.is_empty() {
        return Err(AgentError::ResponseParse("empty LLM response".into()));
    }
    Ok(LlmResponse::Final { answer })
}

fn extract_fence<'a>(text: &'a str, tag: &str) -> Option<(&'a str, &'a str)> {
    let opener = format!("```{tag}");
    let start = text.find(&opener)?;
    let after_open = start + opener.len();
    let after_open = text[after_open..].find('\n').map(|i| after_open + i + 1)?;
    let rest = &text[after_open..];
    let end = rest.find("```")?;
    Some((&text[..start], &rest[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_final_answer() {
        let r = parse_response("The answer is 42.").unwrap();
        let LlmResponse::Final { answer } = r else {
            unreachable!("expected Final, got {r:?}");
        };
        assert_eq!(answer, "The answer is 42.");
    }

    #[test]
    fn parses_tool_calls_fence() {
        let text = "I'll use add.\n```tool_calls\n[{\"id\":\"1\",\"name\":\"add\",\"args\":{\"a\":1,\"b\":2}}]\n```";
        let r = parse_response(text).unwrap();
        let LlmResponse::UseTools { calls } = r else {
            unreachable!("expected UseTools, got {r:?}");
        };
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "add");
        assert_eq!(calls[0].id, "1");
    }

    #[test]
    fn rejects_empty_response() {
        assert!(parse_response("").is_err());
        assert!(parse_response("   ").is_err());
    }

    #[test]
    fn converts_tool_schema_to_llm_tool() {
        let schemas = vec![ToolSchema {
            name: "add".into(),
            description: "Add two numbers".into(),
            args_schema: serde_json::json!({"type": "object"}),
        }];
        let tools = to_autoagents_tools(&schemas);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].function.name, "add");
        assert_eq!(tools[0].tool_type, "function");
    }
}
