//! Pluggable history-compaction strategies for `AgentWorkflow`.
//!
//! The workflow consults a [`MemoryProvider`] before every reasoning turn to
//! decide whether to `continue_as_new` with a compacted state. The provider is
//! configured once per worker via
//! [`AgentWorkerBuilder::memory`](crate::AgentWorkerBuilder::memory); the
//! default is [`SlidingWindowMemory`], which reproduces the legacy hardcoded
//! behavior (drop everything older than the last 20 messages once history
//! exceeds 200).
//!
//! # Determinism contract
//!
//! Implementations run inside the deterministic workflow body. They MUST be:
//!
//! - **Pure**: no I/O, no clocks, no randomness — given the same `AgentState`,
//!   `should_compact` and `compact` must return the same result on every
//!   replay.
//! - **Synchronous**: the workflow body is sync-only. If you need network
//!   memory (vector store, etc.), that work belongs in an activity called by
//!   a future workflow-side `prepare` hook.
//! - **Stateless across calls**: all conversation state lives in `AgentState`
//!   and is restored from event history. Provider instances are configuration
//!   holders only.
//!
//! # Backward compatibility
//!
//! [`SlidingWindowMemory::default`] uses [`DEFAULT_COMPACT_THRESHOLD`] (200)
//! and [`DEFAULT_KEEP_RECENT`] (20), matching the constants that lived in
//! `src/workflow.rs` prior to this module. Workers that do not call
//! `.memory(...)` get this default and behave identically to earlier releases.

use crate::state::{AgentInput, AgentState, Role};

/// Default history length above which [`SlidingWindowMemory`] compacts.
pub const DEFAULT_COMPACT_THRESHOLD: usize = 200;

/// Default number of most-recent messages [`SlidingWindowMemory`] preserves
/// verbatim when compacting.
pub const DEFAULT_KEEP_RECENT: usize = 20;

/// Strategy for deciding when to compact agent history and how to do it.
///
/// See the module-level docs for the determinism contract every implementation
/// must uphold.
pub trait MemoryProvider: std::fmt::Debug + Send + Sync + 'static {
    /// Called every iteration of the agent loop. Return `true` to trigger a
    /// `continue_as_new` with the [`AgentInput`] returned by [`Self::compact`].
    ///
    /// MUST be pure and deterministic — it is replayed verbatim from history.
    fn should_compact(&self, state: &AgentState) -> bool;

    /// Produce the [`AgentInput`] that seeds the next workflow run after
    /// `continue_as_new`. Only called when [`Self::should_compact`] returned
    /// `true`.
    ///
    /// The returned value is serialized into workflow history; its byte shape
    /// IS the compaction. MUST be pure and deterministic.
    fn compact(&self, state: &AgentState) -> AgentInput;
}

/// FIFO sliding-window compaction. Drops everything older than the last
/// `keep_recent` messages once history exceeds `compact_threshold`, prepending
/// a synthetic text summary of the dropped turns to the system prompt.
///
/// This is the default provider and matches the pre-v0.2 hardcoded behavior.
#[derive(Debug, Clone)]
pub struct SlidingWindowMemory {
    compact_threshold: usize,
    keep_recent: usize,
}

impl SlidingWindowMemory {
    /// Construct with [`DEFAULT_COMPACT_THRESHOLD`] / [`DEFAULT_KEEP_RECENT`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            compact_threshold: DEFAULT_COMPACT_THRESHOLD,
            keep_recent: DEFAULT_KEEP_RECENT,
        }
    }

    /// Override the history length at which compaction fires.
    ///
    /// Panics if `n <= keep_recent`, which would trigger compaction every
    /// iteration.
    #[must_use]
    pub fn with_compact_threshold(mut self, n: usize) -> Self {
        assert!(
            n > self.keep_recent,
            "compact_threshold ({}) must be greater than keep_recent ({})",
            n,
            self.keep_recent
        );
        self.compact_threshold = n;
        self
    }

    /// Override the number of messages preserved verbatim after compaction.
    ///
    /// Panics if `n >= compact_threshold`, which would trigger compaction
    /// every iteration.
    #[must_use]
    pub fn with_keep_recent(mut self, n: usize) -> Self {
        assert!(
            n < self.compact_threshold,
            "keep_recent ({}) must be less than compact_threshold ({})",
            n,
            self.compact_threshold
        );
        self.keep_recent = n;
        self
    }

    /// Inspect the current compaction trigger.
    #[must_use]
    pub fn compact_threshold(&self) -> usize {
        self.compact_threshold
    }

    /// Inspect the current keep-recent setting.
    #[must_use]
    pub fn keep_recent(&self) -> usize {
        self.keep_recent
    }
}

impl Default for SlidingWindowMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryProvider for SlidingWindowMemory {
    fn should_compact(&self, state: &AgentState) -> bool {
        state.history.len() > self.compact_threshold
    }

    fn compact(&self, state: &AgentState) -> AgentInput {
        compact_sliding_window(state, self.keep_recent)
    }
}

/// Pure sliding-window compaction kernel.
///
/// Preserves the system prompt, summarizes everything before the last
/// `keep_recent` messages into a synthetic text block appended to the system
/// prompt, and threads through the original `max_turns` / `output_schema`.
///
/// Exposed for advanced users who want to call the kernel directly from a
/// custom [`MemoryProvider`] without re-implementing the summarization format.
pub fn compact_sliding_window(state: &AgentState, keep_recent: usize) -> AgentInput {
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
        output_schema: state.input.output_schema.clone(),
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
    use crate::state::{Message, ToolCall};
    use autoagents_llm::chat::StructuredOutputFormat;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

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

    fn populated_state(turns: u32, schema: Option<StructuredOutputFormat>) -> AgentState {
        let mut state = AgentState::new(AgentInput {
            system_prompt: "sys".into(),
            user_message: "u0".into(),
            max_turns: 50,
            output_schema: schema,
        });
        for i in 1..turns {
            state.history.push(Message::user(format!("u{i}")));
            state.history.push(Message::assistant_text(format!("a{i}")));
        }
        state
    }

    #[test]
    fn sliding_window_default_uses_published_constants() {
        let m = SlidingWindowMemory::default();
        assert_eq!(m.compact_threshold(), DEFAULT_COMPACT_THRESHOLD);
        assert_eq!(m.keep_recent(), DEFAULT_KEEP_RECENT);
    }

    #[test]
    fn sliding_window_should_compact_at_threshold_boundary() {
        // Lower keep_recent first so the threshold setter's invariant check
        // accepts the smaller threshold.
        let m = SlidingWindowMemory::new()
            .with_keep_recent(3)
            .with_compact_threshold(10);
        let mut state = AgentState::new(AgentInput::default());
        // AgentState::new seeds 2 messages (system + user); top up to 10.
        while state.history.len() < 10 {
            state.history.push(Message::user("x"));
        }
        assert!(
            !m.should_compact(&state),
            "len == threshold must not trigger"
        );
        state.history.push(Message::user("x"));
        assert!(m.should_compact(&state), "len > threshold must trigger");
    }

    #[test]
    fn sliding_window_does_not_compact_short_history() {
        let m = SlidingWindowMemory::default();
        let empty = AgentState::default();
        assert!(!m.should_compact(&empty));
        let small = AgentState::new(AgentInput {
            system_prompt: "sys".into(),
            user_message: "hi".into(),
            max_turns: 5,
            output_schema: None,
        });
        assert!(!m.should_compact(&small));
    }

    #[test]
    fn sliding_window_compact_preserves_system_prompt_and_recent_user() {
        let state = populated_state(30, None);
        let m = SlidingWindowMemory::new()
            .with_compact_threshold(50)
            .with_keep_recent(10);
        let compacted = m.compact(&state);
        assert!(compacted.system_prompt.starts_with("sys"));
        assert!(
            compacted
                .system_prompt
                .contains("Prior conversation summary")
        );
        assert_eq!(compacted.max_turns, state.input.max_turns);
    }

    #[test]
    fn sliding_window_compact_preserves_output_schema() {
        let schema = sample_schema();
        let state = populated_state(30, Some(schema.clone()));
        let m = SlidingWindowMemory::new()
            .with_compact_threshold(50)
            .with_keep_recent(10);
        let compacted = m.compact(&state);
        assert_eq!(compacted.output_schema, Some(schema));
    }

    #[test]
    fn sliding_window_compact_with_custom_keep_recent() {
        // Tool-call assistant lines render with their tool names; verify the
        // synthetic summary mentions the dropped tool call when keep_recent
        // excludes it.
        let mut state = AgentState::new(AgentInput {
            system_prompt: "sys".into(),
            user_message: "u0".into(),
            max_turns: 50,
            output_schema: None,
        });
        state
            .history
            .push(Message::assistant_with_tools(vec![ToolCall {
                id: "c1".into(),
                name: "search".into(),
                args: serde_json::json!({}),
            }]));
        for i in 1..20 {
            state.history.push(Message::user(format!("u{i}")));
            state.history.push(Message::assistant_text(format!("a{i}")));
        }
        let m = SlidingWindowMemory::new()
            .with_compact_threshold(100)
            .with_keep_recent(5);
        let compacted = m.compact(&state);
        assert!(
            compacted
                .system_prompt
                .contains("assistant: called tools [search]"),
            "summary should mention dropped tool call, got: {}",
            compacted.system_prompt
        );
    }

    #[test]
    #[should_panic(expected = "keep_recent")]
    fn sliding_window_panics_on_keep_recent_ge_threshold() {
        let _ = SlidingWindowMemory::new()
            .with_compact_threshold(50)
            .with_keep_recent(100);
    }

    #[test]
    #[should_panic(expected = "compact_threshold")]
    fn sliding_window_panics_on_threshold_le_keep_recent() {
        let _ = SlidingWindowMemory::new()
            .with_keep_recent(50)
            .with_compact_threshold(10);
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

    #[derive(Debug, Default)]
    struct PassthroughMemory {
        invoked: AtomicBool,
    }

    impl MemoryProvider for PassthroughMemory {
        fn should_compact(&self, _state: &AgentState) -> bool {
            self.invoked.store(true, Ordering::SeqCst);
            false
        }

        fn compact(&self, state: &AgentState) -> AgentInput {
            state.input.clone()
        }
    }

    #[test]
    fn custom_memory_provider_compiles_as_arc_dyn() {
        // Compile-only — confirms the trait stays dyn-compatible.
        let _: Arc<dyn MemoryProvider> = Arc::new(PassthroughMemory::default());
    }

    #[test]
    fn custom_memory_provider_is_invoked_via_arc_dyn() {
        let provider: Arc<dyn MemoryProvider> = Arc::new(PassthroughMemory::default());
        let state = AgentState::default();
        assert!(!provider.should_compact(&state));
        // Downcast not possible through `dyn`, but the side effect on the
        // inner struct is observable via a separate Arc to the same impl.
        let owned = Arc::new(PassthroughMemory::default());
        let typed: Arc<dyn MemoryProvider> = owned.clone();
        let _ = typed.should_compact(&state);
        assert!(owned.invoked.load(Ordering::SeqCst));
    }
}
