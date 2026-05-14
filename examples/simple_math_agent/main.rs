#![allow(clippy::large_futures)]
//! Minimal durable agent demo.
//!
//! Registers a single computational tool (`add`) on the worker and starts a
//! workflow with a concrete prompt the agent can answer autonomously. No
//! human-in-the-loop, no stdin plumbing, no signals — just the basic ReAct
//! loop running deterministically inside a Temporal workflow.
//!
//! For the human-in-the-loop variant (an `ask_user` tool that blocks on
//! stdin), see `examples/interactive_math_agent`.
//!
//! Run with three terminals:
//!
//! ```bash
//! # Terminal 1: local Temporal server
//! temporal server start-dev
//!
//! # Terminal 2: worker
//! OPENAI_API_KEY=sk-... cargo run --example simple_math_agent -- worker
//!
//! # Terminal 3: client (starts a workflow and waits for the result)
//! cargo run --example simple_math_agent -- client
//! ```
//!
//! To see durability in action, kill the worker mid-loop and restart it —
//! the workflow resumes from the last completed activity without re-paying
//! for prior LLM turns.

use std::sync::Arc;

use async_trait::async_trait;
use autoagents_core::tool::{ToolCallError, ToolInputT, ToolRuntime};
use autoagents_derive::{ToolInput, tool};
use autoagents_llm::backends::openai::OpenAI;
use autoagents_llm::builder::LLMBuilder;
use autoagents_llm::chat::ReasoningEffort;
use serde::Deserialize;
use serde_json::{Value, json};
use temporal_agent_rs::prelude::*;
use temporalio_client::{
    Client, ClientOptions, Connection, WorkflowGetResultOptions, WorkflowQueryOptions,
    WorkflowStartOptions, envconfig::LoadClientConfigProfileOptions,
};
use temporalio_common::telemetry::TelemetryOptions;
use temporalio_sdk_core::{CoreRuntime, RuntimeOptions};

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
        .model("gemma4:e4b")
        .reasoning(true)
        .reasoning_effort(ReasoningEffort::High)
        .build()?;

    let runtime_opts = RuntimeOptions::builder()
        .telemetry_options(TelemetryOptions::builder().build())
        .build()
        .map_err(|e| anyhow::anyhow!("build runtime options: {e}"))?;
    let runtime = CoreRuntime::new_assume_tokio(runtime_opts)?;

    let mut worker = AgentWorkerBuilder::new(client)
        .llm(llm)
        .tool(Arc::new(Add))
        .queue("agents")
        .build_worker(&runtime)?;

    tracing::info!("worker started on queue 'agents'");
    worker.run().await?;
    Ok(())
}

async fn run_client(client: Client) -> anyhow::Result<()> {
    let input = AgentInput {
        system_prompt:
            "You are a math assistant. You can call tools to compute. Output answer in words."
                .into(),
        user_message: "17.5 + 4.2".into(),
        max_turns: 8,
        output_schema: None,
    };

    let handle = client
        .start_workflow(
            AgentWorkflow::run,
            input,
            WorkflowStartOptions::new("agents", "simple-math-demo-1").build(),
        )
        .await?;

    tracing::info!(
        workflow_id = "simple-math-demo-1",
        "started workflow; awaiting result"
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
    Ok(())
}

/// Inspect the running workflow's live state. Useful while debugging.
async fn run_status(client: Client) -> anyhow::Result<()> {
    let handle = client.get_workflow_handle::<AgentWorkflow>("simple-math-demo-1".to_string());
    let state: AgentState = handle
        .query(
            AgentWorkflow::get_state,
            (),
            WorkflowQueryOptions::default(),
        )
        .await?;
    println!("turn        : {}", state.turn);
    println!("tool_calls  : {}", state.tool_calls_executed);
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
