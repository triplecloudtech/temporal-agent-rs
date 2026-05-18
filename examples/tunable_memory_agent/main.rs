#![allow(clippy::large_futures)]
//! Pluggable memory backend demo.
//!
//! Same ReAct loop as `simple_math_agent`, but the worker passes a
//! deliberately aggressive [`SlidingWindowMemory`] config to
//! [`AgentWorkerBuilder::memory`] so you can observe history compaction
//! firing on a short conversation instead of waiting for the 200-message
//! default to kick in.
//!
//! ## What to watch
//!
//! Compaction triggers when `AgentState::history.len() > compact_threshold`.
//! This example uses `compact_threshold = 10` and `keep_recent = 4`, so after
//! roughly four reasoning turns (system + user + a handful of
//! assistant/tool exchanges) the workflow will:
//!
//! 1. Build a synthetic summary of older messages,
//! 2. Call `continue_as_new` with that summary prepended to the system
//!    prompt and only the most recent 4 messages retained,
//! 3. Resume with a fresh event history (and the turn counter reset to 0).
//!
//! Inspect this via `status` while the workflow is running. After a
//! `continue_as_new` the Temporal Web UI will show a new run with a much
//! shorter event history and the new run's `input.system_prompt` will
//! contain the `"[Prior conversation summary, N messages dropped]"` block.
//!
//! ## Writing your own provider
//!
//! [`KeepEverythingMemory`] below is a complete custom `MemoryProvider`
//! implementation in 8 lines. Run the worker with `KEEP_EVERYTHING=1` to
//! activate it — useful for short-lived debugging sessions where you don't
//! want compaction at all. (Don't ship this to production: event history
//! grows unbounded.)
//!
//! Run with three terminals:
//!
//! ```bash
//! # Terminal 1: local Temporal server
//! temporal server start-dev
//!
//! # Terminal 2: worker
//! OPENAI_API_KEY=sk-... cargo run --example tunable_memory_agent -- worker
//!
//! # Terminal 3: client (multi-step prompt that should trigger compaction)
//! cargo run --example tunable_memory_agent -- client
//!
//! # Terminal 3 again, while the workflow is mid-flight:
//! cargo run --example tunable_memory_agent -- status
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use autoagents_core::tool::{ToolCallError, ToolInputT, ToolRuntime};
use autoagents_derive::{ToolInput, tool};
use autoagents_llm::backends::openai::OpenAI;
use autoagents_llm::builder::LLMBuilder;
use serde::Deserialize;
use serde_json::{Value, json};
use temporal_agent_rs::prelude::*;
use temporalio_client::{
    Client, ClientOptions, Connection, WorkflowGetResultOptions, WorkflowQueryOptions,
    WorkflowStartOptions, envconfig::LoadClientConfigProfileOptions,
};
use temporalio_common::telemetry::TelemetryOptions;
use temporalio_sdk_core::{CoreRuntime, RuntimeOptions};

const WORKFLOW_ID: &str = "tunable-memory-demo-1";

#[derive(Deserialize, ToolInput)]
struct AddArgs {
    #[input(description = "First addend")]
    a: f64,
    #[input(description = "Second addend")]
    b: f64,
}

#[tool(name = "add", description = "Add two numbers", input = AddArgs)]
#[derive(Default, Clone)]
struct Add;

#[async_trait]
impl ToolRuntime for Add {
    async fn execute(&self, args: Value) -> Result<Value, ToolCallError> {
        let parsed: AddArgs = serde_json::from_value(args)?;
        Ok(json!({ "sum": parsed.a + parsed.b }))
    }
}

/// Example custom [`MemoryProvider`] that never compacts.
///
/// Useful for short debugging runs where you want the full conversation
/// to survive in workflow history. NOT suitable for long-running
/// workflows — event history will grow unbounded.
///
/// Implementations MUST be pure and synchronous: `should_compact` and
/// `compact` are invoked inside the deterministic workflow body and have
/// to return identical results on every replay for the same `AgentState`.
#[derive(Debug)]
struct KeepEverythingMemory;

impl MemoryProvider for KeepEverythingMemory {
    fn should_compact(&self, _state: &AgentState) -> bool {
        false
    }

    fn compact(&self, state: &AgentState) -> AgentInput {
        // Never called (should_compact always returns false), but the trait
        // requires an impl. Returning the original input is the safest
        // no-op.
        state.input.clone()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,temporal_agent_rs=debug".into()),
        )
        .init();

    let mode = std::env::args().nth(1).unwrap_or_else(|| "worker".into());
    let client = connect().await?;

    match mode.as_str() {
        "worker" => run_worker(client).await,
        "client" => run_client(client).await,
        "status" => run_status(client).await,
        other => Err(anyhow::anyhow!(
            "unknown mode '{other}', expected one of: worker | client | status"
        )),
    }
}

async fn connect() -> anyhow::Result<Client> {
    let (conn_opts, client_opts) =
        ClientOptions::load_from_config(LoadClientConfigProfileOptions::default())
            .map_err(|e| anyhow::anyhow!("load client config: {e}"))?;
    let connection = Connection::connect(conn_opts).await?;
    Ok(Client::new(connection, client_opts)?)
}

async fn run_worker(client: Client) -> anyhow::Result<()> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY must be set for the worker"))?;

    let base_url =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());

    let llm: Arc<dyn LLMProvider> = LLMBuilder::<OpenAI>::new()
        .api_key(api_key)
        .base_url(base_url)
        .model("gpt-4o-mini")
        .build()?;

    let runtime_opts = RuntimeOptions::builder()
        .telemetry_options(TelemetryOptions::builder().build())
        .build()
        .map_err(|e| anyhow::anyhow!("build runtime options: {e}"))?;
    let runtime = CoreRuntime::new_assume_tokio(runtime_opts)?;

    // Aggressive compaction so a short demo conversation actually trips it.
    // Production defaults are 200 / 20; bumping `compact_threshold` down to
    // 10 means continue_as_new will fire after only a few tool exchanges.
    //
    // Set `KEEP_EVERYTHING=1` to swap in the custom never-compact provider
    // instead (see [`KeepEverythingMemory`]).
    let keep_everything = std::env::var("KEEP_EVERYTHING").is_ok();
    let memory: Arc<dyn MemoryProvider> = if keep_everything {
        Arc::new(KeepEverythingMemory)
    } else {
        Arc::new(
            SlidingWindowMemory::new()
                .with_keep_recent(4)
                .with_compact_threshold(10),
        )
    };

    let mut worker = AgentWorkerBuilder::new(client)
        .llm(llm)
        .tool(Arc::new(Add))
        .queue("agents")
        .memory(memory)
        .build_worker(&runtime)?;

    if keep_everything {
        tracing::info!("worker started on queue 'agents' (KeepEverythingMemory — no compaction)");
    } else {
        tracing::info!(
            compact_threshold = 10,
            keep_recent = 4,
            "worker started on queue 'agents' (aggressive memory compaction)"
        );
    }
    worker.run().await?;
    Ok(())
}

async fn run_client(client: Client) -> anyhow::Result<()> {
    let input = AgentInput {
        system_prompt:
            "You are a meticulous math assistant. You can call the `add` tool to compute. \
             Call the tool ONE addition at a time — never combine multiple operations into one \
             call. After each tool result, state the running total before the next call. \
             When all additions are done, output the final answer in words."
                .into(),
        // Chain of additions chosen so the model is forced to issue several
        // sequential tool calls, each appending an assistant+tool pair to
        // history — enough exchanges to trip a 10-message compact_threshold.
        user_message: "Compute step by step: 1.5 + 2.5, then add 3, then add 4.25, then add 5.75. \
                       Give me the final sum."
            .into(),
        max_turns: 12,
        output_schema: None,
    };

    let handle = client
        .start_workflow(
            AgentWorkflow::run,
            input,
            WorkflowStartOptions::new("agents", WORKFLOW_ID).build(),
        )
        .await?;

    tracing::info!(
        workflow_id = WORKFLOW_ID,
        "started workflow; awaiting result (compaction will fire mid-run)"
    );

    let out: AgentOutput = handle
        .get_result(WorkflowGetResultOptions::default())
        .await?;
    println!();
    println!("=== AgentOutput ===");
    println!("final_answer : {}", out.final_answer);
    println!("stop_reason  : {:?}", out.stop_reason);
    println!("turns_used   : {}", out.turns_used);
    println!("tool_calls   : {}", out.tool_calls);
    println!();
    println!("Note: turns_used / tool_calls reflect ONLY the post-compaction run.");
    println!("Earlier turns were folded into the system prompt summary.");
    Ok(())
}

/// Inspect the running workflow's live state. Useful for catching the
/// continue-as-new boundary: before compaction `history.len()` grows past
/// 10; immediately after, it drops to `keep_recent (4) + system + user`
/// and `turn` resets to 0.
async fn run_status(client: Client) -> anyhow::Result<()> {
    let handle = client.get_workflow_handle::<AgentWorkflow>(WORKFLOW_ID.to_string());
    let state: AgentState = handle
        .query(
            AgentWorkflow::get_state,
            (),
            WorkflowQueryOptions::default(),
        )
        .await?;
    println!("turn         : {}", state.turn);
    println!("tool_calls   : {}", state.tool_calls_executed);
    println!("history.len(): {}", state.history.len());
    let sys = state.input.system_prompt.as_str();
    let has_summary = sys.contains("Prior conversation summary");
    println!(
        "compacted?   : {}",
        if has_summary { "yes" } else { "no (yet)" }
    );
    println!("history (last 4):");
    for msg in state.history.iter().rev().take(4).rev() {
        let preview = if msg.content.len() > 120 {
            format!("{}…", &msg.content[..120])
        } else {
            msg.content.clone()
        };
        println!("  [{:?}] {preview}", msg.role);
    }
    Ok(())
}
