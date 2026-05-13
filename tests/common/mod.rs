//! Shared helpers for integration tests.
//!
//! Spins up Temporal and Ollama in disposable containers using
//! [`testcontainers`]. Both containers are cleaned up when their handles drop.

use std::time::Duration;

use testcontainers::{
    GenericImage, ImageExt,
    core::{
        CmdWaitFor, ContainerAsync, ExecCommand, IntoContainerPort, WaitFor, wait::HttpWaitStrategy,
    },
    runners::AsyncRunner,
};

/// gRPC port Temporal exposes inside the container.
pub const TEMPORAL_GRPC_PORT: u16 = 7233;

/// HTTP port Ollama exposes inside the container.
pub const OLLAMA_HTTP_PORT: u16 = 11434;

/// Start an ephemeral Temporal dev server. Uses `temporalio/temporal`
/// (the CLI image) with `server start-dev` for an all-in-one SQLite-backed
/// server with a single process.
///
/// Returns the container handle (must be held to keep it alive) and a
/// `host:port` string suitable for the Temporal client `target` field.
pub async fn start_temporal() -> anyhow::Result<(ContainerAsync<GenericImage>, String)> {
    let container = GenericImage::new("temporalio/temporal", "latest")
        .with_exposed_port(TEMPORAL_GRPC_PORT.tcp())
        .with_wait_for(WaitFor::message_on_stdout("Temporal Metrics:"))
        .with_cmd([
            "server",
            "start-dev",
            "--ip",
            "0.0.0.0",
            "--namespace",
            "default",
        ])
        .with_startup_timeout(Duration::from_secs(90))
        .start()
        .await?;

    let host = container.get_host().await?;
    let port = container
        .get_host_port_ipv4(TEMPORAL_GRPC_PORT.tcp())
        .await?;
    Ok((container, format!("{host}:{port}")))
}

/// Start an Ollama container and pull the requested model into it.
///
/// `model` is an Ollama model tag (e.g. `"qwen2.5:0.5b"`). The pull happens
/// inside the container via `ollama pull`; this can take 30-120 seconds on
/// first run depending on bandwidth and model size.
///
/// Returns the container handle and a base URL `http://host:port` suitable
/// for the OpenAI-compatible API.
pub async fn start_ollama_with_model(
    model: &str,
) -> anyhow::Result<(ContainerAsync<GenericImage>, String)> {
    let container = GenericImage::new("ollama/ollama", "latest")
        .with_exposed_port(OLLAMA_HTTP_PORT.tcp())
        .with_wait_for(WaitFor::http(
            HttpWaitStrategy::new("/api/tags")
                .with_port(OLLAMA_HTTP_PORT.tcp())
                .with_expected_status_code(200u16),
        ))
        .with_startup_timeout(Duration::from_mins(2))
        .start()
        .await?;

    // Pull the model via Ollama's exec API. `CmdWaitFor::exit()` blocks
    // until the pull completes; this can take 30-120s on first run.
    let pull = container
        .exec(
            ExecCommand::new(["ollama", "pull", model])
                .with_cmd_ready_condition(CmdWaitFor::exit()),
        )
        .await?;
    let exit_code = pull.exit_code().await?;
    if exit_code != Some(0) {
        anyhow::bail!("ollama pull {model} exited with {exit_code:?}");
    }

    let host = container.get_host().await?;
    let port = container.get_host_port_ipv4(OLLAMA_HTTP_PORT.tcp()).await?;
    Ok((container, format!("http://{host}:{port}")))
}
