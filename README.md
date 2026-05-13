# temporal-agent-rs

[![crates.io](https://img.shields.io/crates/v/temporal-agent-rs.svg)](https://crates.io/crates/temporal-agent-rs)
[![docs.rs](https://img.shields.io/docsrs/temporal-agent-rs)](https://docs.rs/temporal-agent-rs)
[![CI](https://github.com/triplecloudtech/temporal-agent-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/triplecloudtech/temporal-agent-rs/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE.txt)
[![MSRV](https://img.shields.io/badge/rustc-1.95+-blue.svg)](https://blog.rust-lang.org/)

> **Status:** `0.1.0` — early; APIs may break before 1.0.

Durable AI agent execution on [Temporal](https://temporal.io) using
[AutoAgents](https://github.com/liquidos-ai/AutoAgents) for LLM provider and
tool abstractions.

The headline export is `AgentWorkflow`: a Temporal workflow that runs a
ReAct-style agent loop where every LLM call and every tool invocation is
checkpointed as a Temporal activity. If the worker crashes mid-loop, the
workflow resumes from the last completed activity without re-paying for prior
LLM tokens.

> Inspired by Temporal's blog post,
> [*Of Course You Can Build Dynamic AI Agents with Temporal*](https://temporal.io/blog/of-course-you-can-build-dynamic-ai-agents-with-temporal).

## Architecture

```
┌──────────────────────── AgentWorkflow (deterministic) ────────────────────────┐
│                                                                                │
│   while not done:                                                              │
│      ┌──────────┐         ┌─────────────┐                                      │
│      │ history  │ ───────▶│  llm_chat   │ ── LlmResponse ─┐                    │
│      └──────────┘         │  (activity) │                 │                    │
│                           └─────────────┘                 ▼                    │
│                                                  ┌────────────────┐            │
│                                                  │ Final?  Tools? │            │
│                                                  └────────────────┘            │
│                                                       │       │                │
│                                                  return       ▼                │
│                                                          ┌─────────────┐       │
│                                                          │ execute_tool│       │
│                                                          │  (activity) │ × N   │
│                                                          └─────────────┘       │
│                                                                                │
└────────────────────────────────────────────────────────────────────────────────┘
```

- **Workflow** = orchestration. Deterministic, replayable, holds the
  conversation history.
- **Activities** = the only place LLM providers and tool implementations are
  called. Non-deterministic, retryable, observable in the Temporal UI.

## Features

- `AgentWorkflow` with a ReAct loop, signals, queries, and `continue_as_new`
  history compaction.
- `AgentActivities` with `llm_chat` and `execute_tool`.
- `ToolRegistry` that accepts any AutoAgents `Arc<dyn ToolT>` (use the
  `#[tool]` derive macro).
- `AgentWorkerBuilder` for one-line worker setup.
- Provider-agnostic: bring your own `Arc<dyn LLMProvider>` (OpenAI,
  Anthropic, Ollama, etc. — anything supported by `autoagents_llm`).
- **Human-in-the-loop as a regular tool** — the library does not
  special-case any tool name. See
  [Human-in-the-loop tools](#human-in-the-loop-tools).

## Prerequisites

- **Rust ≥ 1.95** ([install via rustup](https://rustup.rs)).
- **Temporal CLI** — for running the examples against a local dev server.
  Install with `brew install temporal` or follow the [official install
  guide](https://docs.temporal.io/cli#install).
- **Docker** — only required to run the integration test suite, which
  spins up Temporal and Ollama containers automatically via
  [`testcontainers`](https://crates.io/crates/testcontainers).
- An OpenAI-compatible API key for the examples (set `OPENAI_API_KEY`;
  override the endpoint with `OPENAI_BASE_URL` to point at Ollama or any
  other compatible server).

## Quick start

```rust
use std::sync::Arc;
use temporal_agent_rs::prelude::*;

# async fn run(
#     client: temporalio_client::Client,
#     llm: Arc<dyn LLMProvider>,
#     my_tool: Arc<dyn ToolT>,
# ) -> anyhow::Result<()> {
let runtime = temporalio_sdk::CoreRuntime::new_assume_tokio(Default::default())?;
let mut worker = AgentWorkerBuilder::new(client)
    .llm(llm)
    .tool(my_tool)
    .queue("agents")
    .build_worker(&runtime)?;
worker.run().await?;
# Ok(())
# }
```

Starting a workflow from a client:

```rust,ignore
use temporal_agent_rs::prelude::*;
use temporalio_client::{WorkflowGetResultOptions, WorkflowStartOptions};

let handle = client.start_workflow(
    AgentWorkflow::run,
    AgentInput {
        system_prompt: "You are a math assistant.".into(),
        user_message: "What is 17.5 + 4.2?".into(),
        max_turns: 5,
    },
    WorkflowStartOptions::new("agents", "math-1").build(),
).await?;

let out: AgentOutput = handle.get_result(WorkflowGetResultOptions::default()).await?;
println!("{}", out.final_answer);
```

## Running the examples

Two examples ship with the crate:

- `simple_math_agent` — minimal autonomous loop with a single `add` tool.
- `interactive_math_agent` — adds an `ask_user` tool so the agent can pause
  for human input on the worker's stdin.

```bash
# Terminal 1: local Temporal dev server (install via `brew install temporal` or temporal.io)
temporal server start-dev

# Simple autonomous agent — single `add` tool, no human-in-the-loop.
# Terminal 2:
OPENAI_API_KEY=sk-... cargo run --example simple_math_agent -- worker
# Terminal 3:
cargo run --example simple_math_agent -- client

# Same workflow, but the agent can pause to ask the user for missing info.
# The worker terminal also accepts typed answers on stdin.
OPENAI_API_KEY=sk-... cargo run --example interactive_math_agent -- worker
cargo run --example interactive_math_agent -- client
```

The Temporal Web UI is at http://localhost:8233. Click into the workflow to
see every `llm_chat` and `execute_tool` as a separate activity event.

To witness durability: kill the worker mid-loop (`Ctrl-C` in terminal 2),
restart it, and the workflow picks up from the last completed activity.

## Human-in-the-loop tools

The library treats every tool uniformly — there is no built-in "ask the user"
primitive, no `AskUser` response variant, no `awaiting_user` flag baked into
the workflow state. **Pause-and-wait semantics are implemented inside the
user's tool**, not inside the agent loop.

### Why this works without library special-casing

When the LLM emits a tool call, the workflow dispatches it as an
`execute_tool` activity. If that activity's `execute()` blocks on a channel
waiting for an external answer, Temporal happily keeps it in-flight up to the
configured `start_to_close_timeout` (the library default is **1 hour**;
override per-deployment if you need longer). When the answer arrives, the
tool returns it as a normal `serde_json::Value`. The LLM observes it on the
next `llm_chat` turn as a standard tool result. No special workflow code
needed; the diagram above already covers it.

### The pattern

Define a `ToolT` whose `execute()` publishes the question to an out-of-band
channel and awaits an answer. Three concrete delivery mechanisms, in order
of increasing production-readiness:

| Mechanism | When to use | Crash-durable? |
|---|---|---|
| **Stdin → in-process channel** (used in `examples/interactive_math_agent`) | Local dev, single-user demos | No — pending question lost on worker restart |
| **HTTP / Unix socket sidecar** | Multi-user UIs, multi-process clients | No — pending question lost unless persisted externally |
| **Temporal async activity completion** (task token + `client.complete_activity_…`) | Production | **Yes** — survives worker restarts |

The example uses the stdin variant for brevity. Production deployments should
use Temporal async activity completion: the tool persists `(task_token,
question)` to a queue/UI, returns `ActivityError::WillCompleteAsync`, and an
external client completes the activity with the answer later. (This requires
the tool to access the `ActivityContext`, which today means writing the
activity directly rather than going through our `execute_tool` dispatcher — a
future library enhancement.)

### Tool-side snippet (from the example)

```rust,ignore
use tokio::sync::broadcast;
use autoagents_derive::{tool, ToolInput};
use autoagents_core::tool::{ToolRuntime, ToolCallError};

#[derive(serde::Deserialize, ToolInput)]
struct AskUserArgs {
    #[input(description = "The question to put to the user")]
    question: String,
}

#[tool(
    name = "ask_user",
    description = "Ask the human user a follow-up question. The agent will \
                   pause until the user replies.",
    input = AskUserArgs
)]
#[derive(Clone)]
struct AskUserTool {
    answers: broadcast::Sender<String>,
}

#[async_trait::async_trait]
impl ToolRuntime for AskUserTool {
    async fn execute(&self, args: serde_json::Value)
        -> Result<serde_json::Value, ToolCallError>
    {
        let parsed: AskUserArgs = serde_json::from_value(args)?;
        println!(">>> AGENT ASKS: {}", parsed.question);
        let mut rx = self.answers.subscribe();
        let answer = rx.recv().await.map_err(|e| {
            ToolCallError::RuntimeError(Box::new(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                e.to_string(),
            )))
        })?;
        Ok(serde_json::json!({ "answer": answer }))
    }
}
```

Register it like any other tool:

```rust,ignore
let (answer_tx, _) = broadcast::channel::<String>(16);
// spawn a stdin reader (or HTTP listener, etc.) that publishes to answer_tx
AgentWorkerBuilder::new(client)
    .llm(llm)
    .tool(Arc::new(MyComputeTool))
    .tool(Arc::new(AskUserTool::new(answer_tx)))
    .queue("agents")
    .build_worker(&runtime)?;
```

### Activity timeout

The default `start_to_close_timeout` for tool activities is set generously
(1 hour) so that human-in-the-loop tools don't trip the timeout. Tools that
complete quickly are unaffected. See
[`src/workflow.rs`](src/workflow.rs) (`tool_opts`) to tweak it.

### Trade-off note

With the in-process answer mechanisms (stdin, local socket), if the worker
process crashes while a question is pending, the answer channel state is
lost. Temporal will retry the `execute_tool` activity on the new worker; the
tool will reprint the question and ask again. For full crash durability,
use the Temporal async activity completion approach.

## Determinism contract for users

When you write tools and provider configs:

- Tools must be **side-effect-safe-on-retry** by default. Tool errors are
  reported back to the LLM, not retried by Temporal, but infrastructure
  errors do retry up to 3 times.
- The LLM provider must be **`Send + Sync + 'static`**. `Arc<dyn
  LLMProvider>` already satisfies this for AutoAgents' built-in providers.
- Never call your `LLMProvider` or your `ToolT` from inside workflow code.
  The workflow holds tools by name; the only path to invocation is the
  `execute_tool` activity.

## Version compatibility

| Crate | Version |
|-------|---------|
| `temporalio-sdk` | `0.4.x` (prerelease) |
| `autoagents` | `0.3.x` |
| Rust edition | 2024 |
| MSRV | 1.95 |

The Temporal Rust SDK is prerelease; API breaks are expected on minor
version bumps. This crate pins to `0.4.x` for now.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, build/test commands,
and PR conventions. By participating you agree to abide by our
[Code of Conduct](CODE_OF_CONDUCT.md).

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

## Security

Please report vulnerabilities privately — see [SECURITY.md](SECURITY.md).

## License

MIT
