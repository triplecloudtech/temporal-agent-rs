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
use autoagents_llm::backends::openai::OpenAI;
use autoagents_llm::builder::LLMBuilder;
use autoagents_llm::chat::ReasoningEffort;
use schemars::{JsonSchema, schema_for};
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
const DEFAULT_MODEL: &str = "qwen3.5:0.8b";

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

/// Typed view of the schema-constrained final answer the agent must produce.
///
/// `JsonSchema` is derived so the same struct generates the JSON Schema we
/// hand to the model and the type we deserialize the reply into — schema and
/// parser stay in sync by construction. `deny_unknown_fields` emits
/// `additionalProperties: false`, required by OpenAI's strict mode.
#[derive(Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
struct Expression {
    left_operand: f64,
    right_operand: f64,
    result: f64,
    operation: String,
}

#[async_trait]
impl ToolRuntime for Add {
    async fn execute(&self, args: Value) -> Result<Value, ToolCallError> {
        let parsed: AddArgs = serde_json::from_value(args)?;
        Ok(json!({ "sum": parsed.a + parsed.b }))
    }
}

#[tokio::test]
#[ignore = "requires Docker; run with `cargo test --test agent_workflow -- --ignored`"]
#[allow(clippy::too_many_lines)] // sequential container/worker/driver setup; splitting hurts readability
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
    let llm: Arc<dyn LLMProvider> = LLMBuilder::<OpenAI>::new()
        .base_url(format!("{ollama_base_url}/v1"))
        .api_key("dummy")
        .model(model)
        .reasoning(true)
        .reasoning_effort(ReasoningEffort::High)
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
            user_message: "What is 15.4 + 4.5?".into(),
            max_turns: 5,
            output_schema: Some(StructuredOutputFormat {
                name: "expression".into(),
                description: Some("Expression with answer.".into()),
                schema: Some(
                    serde_json::to_value(schema_for!(Expression))
                        .expect("Expression schema serializes"),
                ),
                strict: Some(true),
            }),
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
    // Schema-constrained final answer must parse into the typed shape and
    // carry the exact operands and computed result. Anything else means the
    // model either ignored the schema or hallucinated the math.
    let parsed: Expression = serde_json::from_str(&out.final_answer).unwrap_or_else(|e| {
        panic!(
            "final_answer should parse as Expression: {e}; got: {}",
            out.final_answer
        )
    });
    // Tight epsilon — LLM emits JSON literals so values round-trip exactly,
    // but clippy::float_cmp insists on `abs() < eps` over `==`.
    assert!(
        (parsed.left_operand - 15.4).abs() < f64::EPSILON,
        "left_operand mismatch: got {}",
        parsed.left_operand
    );
    assert!(
        (parsed.right_operand - 4.5).abs() < f64::EPSILON,
        "right_operand mismatch: got {}",
        parsed.right_operand
    );
    assert!(
        (parsed.result - 19.9).abs() < f64::EPSILON,
        "result mismatch: got {}",
        parsed.result
    );
    assert!(
        !parsed.operation.is_empty(),
        "operation should be set (model produces e.g. \"+\" or \"add\")"
    );

    Ok(())
}
