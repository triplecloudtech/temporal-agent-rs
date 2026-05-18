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
- [src/builder.rs](src/builder.rs) — `AgentWorkerBuilder` fluent builder; wires LLM + tools + memory provider into a Temporal `Worker`.
- [src/workflow.rs](src/workflow.rs) — `AgentWorkflow` with `#[run]`, `#[signal] add_user_message`, `#[query] get_state`, `#[query] turn_count`. Owns the ReAct loop.
- [src/activities.rs](src/activities.rs) — `AgentActivities::llm_chat` and `AgentActivities::execute_tool`. The *only* place LLM providers and tool implementations execute.
- [src/llm.rs](src/llm.rs) — translation between local `Message`/`ToolSchema` types and AutoAgents `ChatMessage`/`LlmTool`; native-tool-call parsing with fenced-JSON fallback. The only file that touches `autoagents_llm` types in the hot path (`src/llm.rs:6`).
- [src/state.rs](src/state.rs) — `AgentInput`, `AgentOutput`, `AgentState`, `Message`, `ToolCall`, `ToolResult`, `ToolSchema`, `LlmResponse`, `StopReason`.
- [src/memory.rs](src/memory.rs) — `MemoryProvider` trait, default `SlidingWindowMemory` impl, and the `compact_sliding_window` kernel. Pluggable compaction strategy consulted by the workflow before every turn.
- [src/tool.rs](src/tool.rs) — `ToolRegistry` (immutable name→impl map) and its builder.
- [src/error.rs](src/error.rs) — `AgentError` with `is_retryable()` to distinguish transient vs. permanent.
- [src/prelude.rs](src/prelude.rs) — convenience re-exports including AutoAgents traits (`ToolT`, `LLMProvider`, `ToolRuntime`, `ToolCallError`) and memory types (`MemoryProvider`, `SlidingWindowMemory`).

**Public API surface (what a user actually touches):** `AgentWorkerBuilder`, `AgentWorkflow`, `AgentInput`/`AgentOutput`, `ToolRegistry`, `MemoryProvider`/`SlidingWindowMemory`. Users supply their own `Arc<dyn LLMProvider>` and `Arc<dyn ToolT>` from AutoAgents.

**Non-obvious behaviors to preserve when editing:**

- **History compaction is pluggable.** The workflow consults `MemoryProvider::should_compact` before every turn; on `true` it calls `MemoryProvider::compact` and `continue_as_new` with the returned `AgentInput`. Default provider is `SlidingWindowMemory` (`compact_threshold = 200`, `keep_recent = 20`), preserving the legacy hardcoded behavior. Override via `AgentWorkerBuilder::memory(Arc::new(SlidingWindowMemory::new().with_compact_threshold(N).with_keep_recent(K)))` or supply your own `Arc<dyn MemoryProvider>`. Trait impls MUST be pure and sync — they run inside the deterministic workflow body. The kernel summarizer lives at `src/memory.rs::compact_sliding_window`; any change to the `Message` shape needs to round-trip through it.
- **Tool error semantics.** Tool-side failures return `Ok(ToolResult { error: Some(...) })` so the LLM can see and recover from them (`src/activities.rs:59-88`). Only infrastructure errors (missing tool, serde failure) surface as activity `Err`, which Temporal retries.
- **Process-global worker config (`WORKER_TOOL_CATALOG`, `WORKER_MEMORY`).** Two `OnceCell`s in `src/builder.rs` published by `build_worker`. The deterministic workflow body reads them on every replay, so they must be set before the worker starts and never mutated after. Building a second worker in the same process with a *different* catalog (compared by `PartialEq`) or a different memory `Arc` (compared by `Arc::ptr_eq`) returns `AgentError::Other` — multi-worker setups in one process must share the same `Arc<dyn MemoryProvider>` and register identical tools in the same order.
- **Activity timeouts.** Set inside `AgentWorkflow::run` at `src/workflow.rs:66-74`: LLM activity 120s start-to-close / 30s heartbeat, tool activity **3600s** start-to-close (generous on purpose — supports human-in-the-loop tools that block on stdin/HTTP/async-completion).
- **Mid-conversation user input.** The `add_user_message` signal pushes into `pending_user_messages`, drained at the top of each loop iteration (`src/workflow.rs:145-152`). Don't mutate `history` directly from signal handlers — that races with the in-flight `llm_chat` activity.
- **Dual LLM response parsing.** `src/llm.rs` tries native tool calls first, then falls back to a fenced `\`\`\`tool_calls` JSON block so non-OpenAI providers still work.

**Determinism contract (from the README, repeated because it's load-bearing):**
- Tools must be side-effect-safe on retry.
- `LLMProvider` and `ToolT` impls must be `Send + Sync + 'static`.
- Never invoke `LLMProvider` or `ToolT` from workflow code — only from activities.
- `MemoryProvider` impls must be pure, sync, and stateless (config-only) — `should_compact` and `compact` are called inside the workflow body and must return identical results on replay for the same `AgentState`.

## Documentation maintenance

After any change large enough to alter the public API surface, observable
behavior, defaults, or feature set, update the user-facing docs in the same
PR — stale docs are worse than no docs because they actively mislead.
Specifically:

- **[AGENTS.md](AGENTS.md)** (this file) — update the module map, public API
  surface line, non-obvious behaviors, and determinism contract whenever any
  of them change. Add new module entries here as soon as you create them.
- **[README.md](README.md)** — update the features list, examples list,
  user-facing determinism contract, and any code snippets affected by API
  changes. If you add a feature with its own knobs (caching, fallback,
  memory backends, etc.), give it a short dedicated section like
  "Pluggable memory backends" so users can find it without reading the
  source.
- **[examples/](examples/)** — when adding a new top-level feature, ship
  one runnable example that exercises it (model the new example on
  `simple_math_agent` — same worker/client/status mode template, same
  three-terminal flow). Register it in `Cargo.toml` under a new
  `[[example]]` entry and add a one-line description plus a runnable
  invocation to README.md's "Running the examples" block.

Rule of thumb: if you touched `src/lib.rs` re-exports, `src/prelude.rs`, or
`AgentWorkerBuilder`'s public API, you owe at least one edit to each of the
three above.

## Version pins

`temporalio-sdk` 0.4.x (prerelease), `autoagents` 0.3.7, Rust edition 2024, MSRV 1.95.
