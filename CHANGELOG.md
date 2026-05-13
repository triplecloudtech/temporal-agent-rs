# Changelog

All notable changes to this project are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
with the caveat that the public API is unstable until 1.0.0.

## [Unreleased]

## [0.1.0] - 2026-05-13

Initial public release.

### Added

- `AgentWorkflow` — durable Temporal workflow that runs a ReAct loop
  with checkpointed LLM and tool activities.
- `AgentActivities` — implementations of `llm_chat` and `execute_tool`
  activities that bridge AutoAgents' `LLMProvider` and `ToolT` traits
  into the Temporal worker.
- `AgentWorkerBuilder` — fluent builder for assembling a Temporal worker
  with an LLM provider and a set of tools.
- `ToolRegistry` / `ToolRegistryBuilder` — immutable name → tool
  dispatch map used by `execute_tool`.
- `AgentInput`, `AgentOutput`, `AgentState`, `StopReason`, `Message`,
  `ToolCall`, `ToolResult`, `ToolSchema`, `LlmResponse` — public state
  types serialized through Temporal.
- `AgentError` with `is_retryable()` to distinguish transient
  (network / rate-limit) from permanent (missing tool, parse) failures.
- Native tool-call parsing for OpenAI / Anthropic styles with a
  fenced-JSON fallback for providers that lack native support.
- History compaction via `continue_as_new` at 200 events (keeps last 20
  messages + a system-prompt summary).
- Signal handler `add_user_message` for mid-conversation user input.
- Query handlers `get_state` and `turn_count` for monitoring live
  workflows.
- Re-exports of `ToolT`, `LLMProvider`, `ToolRuntime`, `ToolCallError`
  from AutoAgents under `temporal_agent_rs::prelude` — these are
  intentional public surface.
- Two runnable examples:
  - `simple_math_agent` — autonomous agent with a single `add` tool.
  - `interactive_math_agent` — adds a human-in-the-loop `ask_user` tool
    that blocks the workflow on stdin input.
- End-to-end smoke test (`tests/agent_workflow.rs`) that spins up
  Temporal and Ollama in testcontainers and runs the full ReAct loop
  against a small local model.

### Infrastructure

- CI (GitHub Actions): rustfmt, clippy (`-D warnings`), unit tests,
  integration tests with Docker, rustdoc with `-D warnings`, MSRV
  (1.95.0) build.
- Scheduled supply-chain audit: `cargo-audit` and `cargo-deny` weekly
  and on dependency changes.
- `deny.toml` license allowlist; `rust-toolchain.toml` pins stable.
- Dependabot configured for cargo and GitHub Actions, weekly.

[Unreleased]: https://github.com/triplecloudtech/temporal-agent-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/triplecloudtech/temporal-agent-rs/releases/tag/v0.1.0
