//! End-to-end smoke test for `AgentWorkflow`.
//!
//! Spins up Temporal and Ollama in disposable containers, registers the
//! library's workflow + activities with a small open-source LLM and a
//! single `add` tool, then runs the ReAct loop against a simple math
//! question. Asserts the loop terminated cleanly with a final answer.
//!
//! Requires Docker. The Ollama model is pulled inside the container on
//! first run; expect 30-90s of cold-start cost.

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use autoagents_core::tool::{ToolCallError, ToolInputT, ToolRuntime};
use autoagents_derive::{ToolInput, tool};
use autoagents_llm::LLMProvider;
use autoagents_llm::backends::ollama::Ollama;
use autoagents_llm::builder::LLMBuilder;
use serde::Deserialize;
use serde_json::{Value, json};
use temporal_agent_rs::prelude::*;
use temporalio_client::{
    Client, ClientOptions, Connection, ConnectionOptions, WorkflowGetResultOptions,
    WorkflowStartOptions,
};
use temporalio_common::telemetry::TelemetryOptions;
use temporalio_sdk_core::{CoreRuntime, RuntimeOptions};
use url::Url;

/// Default Ollama model. Override via `TEMPORAL_AGENT_TEST_MODEL`.
const DEFAULT_MODEL: &str = "qwen2.5:0.5b";

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

#[tokio::test]
#[ignore = "requires Docker; run with `cargo test --test agent_workflow -- --ignored`"]
async fn agent_workflow_completes_against_ollama() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,temporal_agent_rs=debug".into()),
        )
        .try_init();

    let model =
        std::env::var("TEMPORAL_AGENT_TEST_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());

    // Bring up containers in parallel.
    let (temporal_setup, ollama_setup) = tokio::join!(
        common::start_temporal(),
        common::start_ollama_with_model(&model)
    );
    let (_temporal, temporal_target) = temporal_setup?;
    let (_ollama, ollama_base_url) = ollama_setup?;

    // Temporal client.
    let conn_opts =
        ConnectionOptions::new(Url::parse(&format!("http://{temporal_target}"))?).build();
    let connection = Connection::connect(conn_opts).await?;
    let client_opts = ClientOptions::new("default").build();
    let client = Client::new(connection, client_opts)?;

    // Ollama LLM provider.
    let llm: Arc<dyn LLMProvider> = LLMBuilder::<Ollama>::new()
        .base_url(ollama_base_url)
        .model(model)
        .build()?;

    // Worker.
    let runtime_opts = RuntimeOptions::builder()
        .telemetry_options(TelemetryOptions::builder().build())
        .build()
        .map_err(|e| anyhow::anyhow!("build runtime options: {e}"))?;
    let runtime = CoreRuntime::new_assume_tokio(runtime_opts)?;

    let queue = "agents-test";
    let mut worker = AgentWorkerBuilder::new(client.clone())
        .llm(llm)
        .tool(Arc::new(Add))
        .queue(queue)
        .build_worker(&runtime)?;

    // Run worker concurrently with the workflow driver. The Temporal Worker
    // is !Send, so we cannot `tokio::spawn` it onto a multi-thread runtime;
    // tokio::select! keeps both futures on the current task.
    let driver = async {
        let input = AgentInput {
            system_prompt: "You are a math assistant. Use the `add` tool to compute sums. \
                            Reply with the result in plain text."
                .into(),
            user_message: "What is 17.5 + 4.2?".into(),
            max_turns: 5,
        };

        let workflow_id = format!("agent-test-{}", uuid::Uuid::new_v4());
        let handle = client
            .start_workflow(
                AgentWorkflow::run,
                input,
                WorkflowStartOptions::new(queue, &workflow_id).build(),
            )
            .await?;

        let out: AgentOutput = tokio::time::timeout(
            Duration::from_mins(3),
            handle.get_result(WorkflowGetResultOptions::default()),
        )
        .await
        .map_err(|_| anyhow::anyhow!("workflow result timed out after 3 minutes"))??;

        Ok::<AgentOutput, anyhow::Error>(out)
    };

    tokio::pin!(driver);
    let worker_run = worker.run();
    tokio::pin!(worker_run);

    let out = tokio::select! {
        biased;
        res = &mut driver => res?,
        res = &mut worker_run => {
            anyhow::bail!("worker exited before workflow completed: {res:?}");
        }
    };

    // Loose assertions: real LLM output, exact content varies.
    assert_eq!(
        out.stop_reason,
        StopReason::FinalAnswer,
        "agent should have produced a final answer; got {:?} after {} turns",
        out.stop_reason,
        out.turns_used
    );
    assert!(out.turns_used >= 1, "should have taken at least one turn");
    assert!(
        out.turns_used <= 5,
        "should respect max_turns; got {}",
        out.turns_used
    );
    assert!(
        out.final_answer.contains("21.7"),
        "answer should be correct"
    );

    Ok(())
}
