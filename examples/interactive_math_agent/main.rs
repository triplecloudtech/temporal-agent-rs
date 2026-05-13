#![allow(clippy::large_futures)]
//! End-to-end durability + human-in-the-loop demo.
//!
//! For the minimal autonomous variant (single computational tool, no user
//! interaction) see `examples/simple_math_agent`.
//!
//! Two tools are registered on the worker:
//!
//! - `add` — a normal computational tool.
//! - `ask_user` — a user-side **human-in-the-loop** tool whose `execute()`
//!   blocks until a human types an answer into the worker's stdin. The
//!   library does not special-case it; from the workflow's perspective it
//!   is just another tool invocation, dispatched as an `execute_tool`
//!   activity that happens to take a long time.
//!
//! Run with three terminals:
//!
//! ```bash
//! # Terminal 1: local Temporal server
//! temporal server start-dev
//!
//! # Terminal 2: worker. Watch this terminal — when the agent asks a
//! # follow-up question, you reply by typing the answer + ENTER here.
//! OPENAI_API_KEY=sk-... cargo run --example interactive_math_agent -- worker
//!
//! # Terminal 3: client (starts a workflow and waits for the result)
//! cargo run --example interactive_math_agent -- client
//! ```
//!
//! To see durability in action, kill the worker mid-loop and restart it —
//! the workflow resumes from the last completed activity without re-paying
//! for prior LLM turns. Note: in-process answer state (the stdin channel)
//! is NOT durable; if you Ctrl-C the worker while a question is pending,
//! the activity retries on restart and reprints the question. For
//! crash-durable human-in-the-loop, see the README.

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
use tokio::io::AsyncBufReadExt;
use tokio::sync::broadcast;

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

/// Argument schema for the `ask_user` tool.
#[derive(Deserialize, ToolInput)]
struct AskUserArgs {
    #[input(description = "The question to put to the user")]
    question: String,
}

/// A user-defined tool that blocks the agent loop on human input.
///
/// The library treats this like any other tool — the workflow dispatches it
/// via the `execute_tool` activity, and the activity stays in-flight (up to
/// the configured `start_to_close_timeout`, default 1h) while
/// `execute()` awaits an answer on the broadcast channel.
#[tool(
    name = "ask_user",
    description = "Ask the human user a follow-up question. The agent will pause until the user replies. Use whenever you need information not present in the conversation.",
    input = AskUserArgs
)]
#[derive(Clone)]
struct AskUserTool {
    answers: broadcast::Sender<String>,
}

impl AskUserTool {
    fn new(answers: broadcast::Sender<String>) -> Self {
        Self { answers }
    }
}

#[async_trait]
impl ToolRuntime for AskUserTool {
    async fn execute(&self, args: Value) -> Result<Value, ToolCallError> {
        let parsed: AskUserArgs = serde_json::from_value(args)?;
        // Surface the question prominently in the worker terminal.
        println!("\n>>> AGENT ASKS: {}", parsed.question);
        println!(">>> type an answer + ENTER:");
        let mut rx = self.answers.subscribe();
        let answer = rx.recv().await.map_err(|e| {
            ToolCallError::RuntimeError(Box::new(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                format!("answer channel closed: {e}"),
            )))
        })?;
        Ok(json!({ "answer": answer }))
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
        .model("gpt-oss:20b")
        .build()?;

    let runtime_opts = RuntimeOptions::builder()
        .telemetry_options(TelemetryOptions::builder().build())
        .build()
        .map_err(|e| anyhow::anyhow!("build runtime options: {e}"))?;
    let runtime = CoreRuntime::new_assume_tokio(runtime_opts)?;

    // Stdin → broadcast channel. Every line typed into the worker terminal
    // is fanned out to whichever `AskUserTool::execute` invocations are
    // currently subscribed (typically one at a time).
    let (answer_tx, _) = broadcast::channel::<String>(16);
    let stdin_tx = answer_tx.clone();
    tokio::spawn(async move {
        let stdin = tokio::io::BufReader::new(tokio::io::stdin());
        let mut lines = stdin.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // No subscribers right now is fine — drop silently.
            let _ = stdin_tx.send(trimmed.to_string());
        }
    });

    let ask_tool = AskUserTool::new(answer_tx);

    let mut worker = AgentWorkerBuilder::new(client)
        .llm(llm)
        .tool(Arc::new(Add))
        .tool(Arc::new(ask_tool))
        .queue("agents")
        .build_worker(&runtime)?;

    tracing::info!("worker started on queue 'agents'; type answers + ENTER when prompted");
    worker.run().await?;
    Ok(())
}

async fn run_client(client: Client) -> anyhow::Result<()> {
    let input = AgentInput {
        system_prompt: "You are a math assistant. You can call tools to compute. \
                        If you need information that is not in the conversation, \
                        call the `ask_user` tool to ask a question — do not guess."
            .into(),
        // Open-ended prompt with missing information → forces the model to
        // call `ask_user` rather than fabricate values.
        user_message: "Please add the two numbers I have in mind.".into(),
        max_turns: 8,
    };

    let handle = client
        .start_workflow(
            AgentWorkflow::run,
            input,
            WorkflowStartOptions::new("agents", "interactive-math-demo-1").build(),
        )
        .await?;

    tracing::info!(
        workflow_id = "interactive-math-demo-1",
        "started workflow; awaiting result (type the answer into the worker terminal if it pauses)"
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
    let handle = client.get_workflow_handle::<AgentWorkflow>("interactive-math-demo-1".to_string());
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
