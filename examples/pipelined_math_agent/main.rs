#![allow(clippy::large_futures)]
//! Pipelined LLM provider — durable agent + caching + fallback.
//!
//! Same `add` tool and workflow as `simple_math_agent`, but the
//! [`LLMProvider`] handed to [`AgentWorkerBuilder::llm`] is wrapped with a
//! [`PipelineBuilder`] that adds:
//!
//! - a [`CacheLayer`] (5-minute TTL) so identical chats short-circuit before
//!   any provider call, and
//! - a [`FallbackLayer`] that routes to a secondary OpenAI model when the
//!   primary returns a retriable provider error.
//!
//! The order matters: layers added first sit outermost. With
//! `Pipeline(Cache → Fallback → primary)`:
//!
//! 1. A cache hit short-circuits before any network call.
//! 2. On a cache miss the primary provider is tried.
//! 3. On certain primary errors (see [`default_is_fallbackable`]) the
//!    fallback provider is tried.
//!
//! Retry is intentionally NOT in the pipeline — Temporal's activity
//! [`RetryPolicy`] owns retry semantics for this crate. Layering retry inside
//! one activity attempt would hide failures from Temporal history and
//! amplify rate-limit pressure.
//!
//! Run with three terminals:
//!
//! ```bash
//! # Terminal 1: local Temporal server
//! temporal server start-dev
//!
//! # Terminal 2: worker (primary + fallback both go to OpenAI by default;
//! # override OPENAI_BASE_URL / model names to point elsewhere)
//! OPENAI_API_KEY=sk-... cargo run --example pipelined_math_agent -- worker
//!
//! # Terminal 3: client (starts a workflow and waits for the result)
//! cargo run --example pipelined_math_agent -- client
//! ```
//!
//! To observe the cache layer, run the client twice with the same prompt and
//! watch OpenAI usage / your provider's dashboard — the second run should
//! consume zero LLM tokens.

use std::sync::Arc;
use std::time::Duration;

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

const WORKFLOW_ID: &str = "pipelined-math-demo-1";

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
    let primary_model = std::env::var("OPENAI_MODEL_PRIMARY").unwrap_or_else(|_| "gpt-4o".into());
    let fallback_model =
        std::env::var("OPENAI_MODEL_FALLBACK").unwrap_or_else(|_| "gpt-4o-mini".into());

    if primary_model == fallback_model {
        tracing::warn!(
            primary = %primary_model,
            "OPENAI_MODEL_FALLBACK matches OPENAI_MODEL_PRIMARY — fallback won't \
             observably switch providers; set the env vars to different models to demo"
        );
    }

    let primary: Arc<dyn LLMProvider> = LLMBuilder::<OpenAI>::new()
        .api_key(&api_key)
        .base_url(&base_url)
        .model(&primary_model)
        .build()?;

    let fallback: Arc<dyn LLMProvider> = LLMBuilder::<OpenAI>::new()
        .api_key(&api_key)
        .base_url(&base_url)
        .model(&fallback_model)
        .build()?;

    // Compose the pipeline. First-added layer is outermost.
    //
    //   request → CacheLayer ──(miss)──▶ FallbackLayer ──(try)──▶ primary
    //                                                      │
    //                                                      └─(fail)──▶ fallback
    //
    // Cache wraps the whole composition so a hit short-circuits before any
    // provider is touched; fallback wraps only the primary so it triggers
    // exclusively on a primary error after a cache miss.
    let llm: Arc<dyn LLMProvider> = PipelineBuilder::new(primary)
        .add_layer(CacheLayer::new(CacheConfig {
            ttl: Some(Duration::from_mins(5)),
            ..Default::default()
        }))
        .add_layer(FallbackLayer::new(vec![fallback]))
        .build();

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

    tracing::info!(
        primary = %primary_model,
        fallback = %fallback_model,
        "pipelined worker started on queue 'agents' (cache + fallback active)"
    );
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
            WorkflowStartOptions::new("agents", WORKFLOW_ID).build(),
        )
        .await?;

    tracing::info!(
        workflow_id = WORKFLOW_ID,
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
    let handle = client.get_workflow_handle::<AgentWorkflow>(WORKFLOW_ID.to_string());
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
