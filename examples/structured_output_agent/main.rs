#![allow(clippy::large_futures)]
//! Structured-output durable agent demo.
//!
//! Shows how to attach a JSON Schema (`StructuredOutputFormat`) to an
//! `AgentInput` so the model's final answer is a JSON object the client can
//! deserialize directly into a typed Rust struct. The schema is part of
//! `LlmChatInput` and therefore lives in workflow event history — replay
//! re-issues byte-identical activity invocations.
//!
//! The agent has one tool (`lookup_temperature`) so it has to do real ReAct
//! reasoning before producing the structured final answer. Combining tools
//! with structured outputs in a single conversation is best-supported on
//! OpenAI-compatible providers (`gpt-4o`, `gpt-4o-mini`); other backends may
//! need adjustments.
//!
//! Run with three terminals:
//!
//! ```bash
//! # Terminal 1: local Temporal server
//! temporal server start-dev
//!
//! # Terminal 2: worker
//! OPENAI_API_KEY=sk-... cargo run --example structured_output_agent -- worker
//!
//! # Terminal 3: client (starts a workflow and waits for the result)
//! cargo run --example structured_output_agent -- client
//! ```
//!
//! Override the LLM endpoint with `OPENAI_BASE_URL` for self-hosted models.

use std::sync::Arc;

use async_trait::async_trait;
use autoagents_core::tool::{ToolCallError, ToolInputT, ToolRuntime};
use autoagents_derive::{ToolInput, tool};
use autoagents_llm::backends::openai::OpenAI;
use autoagents_llm::builder::LLMBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use temporal_agent_rs::prelude::*;
use temporalio_client::{
    Client, ClientOptions, Connection, WorkflowGetResultOptions, WorkflowStartOptions,
    envconfig::LoadClientConfigProfileOptions,
};
use temporalio_common::telemetry::TelemetryOptions;
use temporalio_sdk_core::{CoreRuntime, RuntimeOptions};

const WORKFLOW_ID: &str = "structured-output-demo-1";

/// Typed view of the agent's final answer. The model is constrained to
/// produce JSON matching this shape via the schema below.
#[derive(Debug, Deserialize, Serialize)]
struct WeatherReport {
    city: String,
    temperature_c: f64,
    conditions: String,
    source: String,
}

#[derive(Deserialize, ToolInput)]
struct LookupArgs {
    #[input(description = "City to look up the temperature for")]
    city: String,
}

/// Stub tool that returns hardcoded readings for a couple of cities. Returns
/// an error for unknown cities so the model has to either pick a known city
/// or surface the failure in its final answer.
#[tool(
    name = "lookup_temperature",
    description = "Look up the current temperature in Celsius for a known city.",
    input = LookupArgs,
)]
#[derive(Default, Clone)]
struct LookupTemperature;

#[async_trait]
impl ToolRuntime for LookupTemperature {
    async fn execute(&self, args: Value) -> Result<Value, ToolCallError> {
        let parsed: LookupArgs = serde_json::from_value(args)?;
        let (temperature_c, conditions) = match parsed.city.to_ascii_lowercase().as_str() {
            "berlin" => (12.4, "overcast"),
            "tokyo" => (21.7, "partly cloudy"),
            "san francisco" | "sf" => (16.1, "foggy"),
            other => {
                return Err(ToolCallError::RuntimeError(
                    format!("no station data for '{other}'").into(),
                ));
            }
        };
        Ok(json!({
            "temperature_c": temperature_c,
            "conditions": conditions,
        }))
    }
}

/// Build the JSON Schema describing the model's required final-answer shape.
///
/// `additionalProperties: false` and `required` listing every key are
/// mandatory for OpenAI's strict structured-output mode.
fn weather_report_schema() -> StructuredOutputFormat {
    StructuredOutputFormat {
        name: "weather_report".into(),
        description: Some("Structured weather observation for a single city.".into()),
        schema: Some(json!({
            "type": "object",
            "properties": {
                "city":          { "type": "string" },
                "temperature_c": { "type": "number" },
                "conditions":    { "type": "string" },
                "source":        { "type": "string" }
            },
            "required": ["city", "temperature_c", "conditions", "source"],
            "additionalProperties": false
        })),
        strict: Some(true),
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
        other => Err(anyhow::anyhow!(
            "unknown mode '{other}', expected one of: worker | client"
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

    // Use a model that supports JSON-Schema-constrained outputs. Override via
    // `OPENAI_MODEL` if your endpoint exposes a different identifier.
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());

    let llm: Arc<dyn LLMProvider> = LLMBuilder::<OpenAI>::new()
        .api_key(api_key)
        .base_url(base_url)
        .model(model)
        .build()?;

    let runtime_opts = RuntimeOptions::builder()
        .telemetry_options(TelemetryOptions::builder().build())
        .build()
        .map_err(|e| anyhow::anyhow!("build runtime options: {e}"))?;
    let runtime = CoreRuntime::new_assume_tokio(runtime_opts)?;

    let mut worker = AgentWorkerBuilder::new(client)
        .llm(llm)
        .tool(Arc::new(LookupTemperature))
        .queue("agents")
        .build_worker(&runtime)?;

    tracing::info!("worker started on queue 'agents'");
    worker.run().await?;
    Ok(())
}

async fn run_client(client: Client) -> anyhow::Result<()> {
    let input = AgentInput {
        system_prompt: "You are a weather reporter. Use the `lookup_temperature` tool \
                        to fetch live readings for the user's city, then answer with a \
                        single JSON object matching the required schema. Set `source` \
                        to 'lookup_temperature'."
            .into(),
        user_message: "What's the current weather in Tokyo?".into(),
        max_turns: 6,
        output_schema: Some(weather_report_schema()),
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
    println!("stop_reason  : {:?}", out.stop_reason);
    println!("turns_used   : {}", out.turns_used);
    println!("tool_calls   : {}", out.tool_calls);
    println!("raw answer   : {}", out.final_answer);

    match serde_json::from_str::<WeatherReport>(&out.final_answer) {
        Ok(report) => {
            println!();
            println!("=== Parsed WeatherReport ===");
            println!("city          : {}", report.city);
            println!("temperature_c : {}", report.temperature_c);
            println!("conditions    : {}", report.conditions);
            println!("source        : {}", report.source);
        }
        Err(e) => {
            eprintln!();
            eprintln!(
                "warning: final answer did not parse as WeatherReport ({e}). \
                 The provider may not enforce structured outputs end-to-end — \
                 try a different model or check the schema."
            );
        }
    }
    Ok(())
}
