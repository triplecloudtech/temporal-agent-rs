# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, Codex, etc.) when working with code in this repository.

## Project

`temporal-agent-rs` — a Rust library that runs AutoAgents-style ReAct loops as durable Temporal workflows. Every LLM call and every tool invocation is checkpointed as a Temporal activity, so a worker crash mid-loop resumes from the last completed activity without re-paying for prior tokens. See [README.md](./README.md) for the user-facing pitch and human-in-the-loop patterns.

## Commands

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Examples are declared as `[[example]]` entries in the root `Cargo.toml` (not workspace members, not separate crates), so always run them with `cargo run --example <name>`. Running an example end-to-end needs **three terminals**:

```bash
# Terminal 1 — Temporal dev server (Web UI at http://localhost:8233)
temporal server start-dev

# Terminal 2 — worker (registers AgentWorkflow + AgentActivities)
OPENAI_API_KEY=sk-... cargo run --example simple_math_agent -- worker

# Terminal 3 — kick off a workflow execution
cargo run --example simple_math_agent -- client
```

The same examples support `-- status` to query live workflow state. Swap `simple_math_agent` for `interactive_math_agent` to exercise a human-in-the-loop `ask_user` tool that blocks on stdin.

Override the LLM endpoint with `OPENAI_BASE_URL` (defaults to `https://api.openai.com/v1`) for self-hosted models.

## Architecture

**The single most important rule:** the workflow is *deterministic and replayable*; activities are the *only* non-deterministic boundary. The workflow must never call `LLMProvider::chat_with_tools` or `ToolT::execute` directly — both are reached through Temporal activities so they get retried, timed-out, and observed via event history. The workflow holds tools by *name* (string), never by reference. See the comment at `src/workflow.rs:9`.

**Module map:**
- [src/lib.rs](src/lib.rs) — module re-exports.
- [src/builder.rs](src/builder.rs) — `AgentWorkerBuilder` fluent builder; wires LLM + tools into a Temporal `Worker`.
- [src/workflow.rs](src/workflow.rs) — `AgentWorkflow` with `#[run]`, `#[signal] add_user_message`, `#[query] get_state`, `#[query] turn_count`. Owns the ReAct loop.
- [src/activities.rs](src/activities.rs) — `AgentActivities::llm_chat` and `AgentActivities::execute_tool`. The *only* place LLM providers and tool implementations execute.
- [src/llm.rs](src/llm.rs) — translation between local `Message`/`ToolSchema` types and AutoAgents `ChatMessage`/`LlmTool`; native-tool-call parsing with fenced-JSON fallback. The only file that touches `autoagents_llm` types in the hot path (`src/llm.rs:6`).
- [src/state.rs](src/state.rs) — `AgentInput`, `AgentOutput`, `AgentState`, `Message`, `ToolCall`, `ToolResult`, `ToolSchema`, `LlmResponse`, `StopReason`, plus `compact()`.
- [src/tool.rs](src/tool.rs) — `ToolRegistry` (immutable name→impl map) and its builder.
- [src/error.rs](src/error.rs) — `AgentError` with `is_retryable()` to distinguish transient vs. permanent.
- [src/prelude.rs](src/prelude.rs) — convenience re-exports including AutoAgents traits (`ToolT`, `LLMProvider`, `ToolRuntime`, `ToolCallError`).

**Public API surface (what a user actually touches):** `AgentWorkerBuilder`, `AgentWorkflow`, `AgentInput`/`AgentOutput`, `ToolRegistry`. Users supply their own `Arc<dyn LLMProvider>` and `Arc<dyn ToolT>` from AutoAgents.

**Non-obvious behaviors to preserve when editing:**

- **History compaction.** When `AgentState::history.len()` exceeds `CONTINUE_AS_NEW_THRESHOLD = 200` (`src/workflow.rs:36`), the workflow calls `continue_as_new` with a compacted state: summary prepended to the system prompt, last 20 messages kept (`src/state.rs::compact`). Any change to the message shape needs to round-trip through `compact()`.
- **Tool error semantics.** Tool-side failures return `Ok(ToolResult { error: Some(...) })` so the LLM can see and recover from them (`src/activities.rs:59-88`). Only infrastructure errors (missing tool, serde failure) surface as activity `Err`, which Temporal retries.
- **`WORKER_TOOL_CATALOG`.** A process-global `OnceCell` set once at worker init in `build_worker` (`src/builder.rs:34`). The deterministic workflow body reads it on every replay, so it must be set before the worker starts and never mutated after.
- **Activity timeouts.** Set inside `AgentWorkflow::run` at `src/workflow.rs:66-74`: LLM activity 120s start-to-close / 30s heartbeat, tool activity **3600s** start-to-close (generous on purpose — supports human-in-the-loop tools that block on stdin/HTTP/async-completion).
- **Mid-conversation user input.** The `add_user_message` signal pushes into `pending_user_messages`, drained at the top of each loop iteration (`src/workflow.rs:145-152`). Don't mutate `history` directly from signal handlers — that races with the in-flight `llm_chat` activity.
- **Dual LLM response parsing.** `src/llm.rs` tries native tool calls first, then falls back to a fenced `\`\`\`tool_calls` JSON block so non-OpenAI providers still work.

**Determinism contract (from the README, repeated because it's load-bearing):**
- Tools must be side-effect-safe on retry.
- `LLMProvider` and `ToolT` impls must be `Send + Sync + 'static`.
- Never invoke `LLMProvider` or `ToolT` from workflow code — only from activities.

## Version pins

`temporalio-sdk` 0.4.x (prerelease), `autoagents` 0.3.7, Rust edition 2024, MSRV 1.95.
