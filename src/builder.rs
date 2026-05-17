//! Convenience builder for assembling an agent-aware Temporal worker.
//!
//! ```ignore
//! let mut worker = AgentWorkerBuilder::new(client)
//!     .llm(my_llm_provider)
//!     .tool(Arc::new(MyTool::default()))
//!     .queue("agents")
//!     .build_worker(&runtime)?;
//! worker.run().await?;
//! ```

use std::sync::Arc;

use autoagents_core::tool::ToolT;
use autoagents_llm::LLMProvider;
use temporalio_client::Client;
use temporalio_sdk::{Worker, WorkerOptions};
use temporalio_sdk_core::CoreRuntime;
use tokio::sync::OnceCell;

use crate::activities::AgentActivities;
use crate::error::AgentError;
use crate::memory::{MemoryProvider, SlidingWindowMemory};
use crate::state::ToolSchema;
use crate::tool::{ToolRegistry, ToolRegistryBuilder};
use crate::workflow::AgentWorkflow;

/// Tool catalog made available to the workflow side.
///
/// The workflow is read-only with respect to tools — it only needs their
/// schemas (name, description, args_schema) to send to the LLM each turn.
/// Stored in a process-wide `OnceCell` so the workflow's `#[run]` body can
/// access it deterministically (the catalog is set once at worker start, so
/// reads return the same value on every replay).
pub(crate) static WORKER_TOOL_CATALOG: OnceCell<Vec<ToolSchema>> = OnceCell::const_new();

/// Memory provider made available to the workflow side.
///
/// Same replay-safety story as [`WORKER_TOOL_CATALOG`]: set once at worker
/// start, read deterministically from the workflow body on every replay.
pub(crate) static WORKER_MEMORY: OnceCell<Arc<dyn MemoryProvider>> = OnceCell::const_new();

/// Fluent builder for an agent-aware Temporal [`Worker`].
///
/// Required calls: [`AgentWorkerBuilder::new`] → [`AgentWorkerBuilder::llm`]
/// → [`AgentWorkerBuilder::build_worker`]. Tools and queue name are
/// optional.
pub struct AgentWorkerBuilder {
    client: Client,
    llm: Option<Arc<dyn LLMProvider>>,
    tools: ToolRegistryBuilder,
    queue: String,
    memory: Option<Arc<dyn MemoryProvider>>,
}

impl AgentWorkerBuilder {
    /// Start a new builder bound to an already-connected Temporal [`Client`].
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self {
            client,
            llm: None,
            tools: ToolRegistry::builder(),
            queue: "agents".to_string(),
            memory: None,
        }
    }

    /// Required. The LLM provider used by the `llm_chat` activity.
    #[must_use]
    pub fn llm(mut self, llm: Arc<dyn LLMProvider>) -> Self {
        self.llm = Some(llm);
        self
    }

    /// Register a tool. Call once per tool the agent should be allowed to use.
    #[must_use]
    pub fn tool(mut self, tool: Arc<dyn ToolT>) -> Self {
        self.tools = self.tools.add(tool);
        self
    }

    /// Override the default task queue name (`"agents"`).
    #[must_use]
    pub fn queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = queue.into();
        self
    }

    /// Override the memory provider. Defaults to
    /// [`SlidingWindowMemory::default`], which preserves the legacy hardcoded
    /// compaction behavior.
    #[must_use]
    pub fn memory(mut self, memory: Arc<dyn MemoryProvider>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Construct the Temporal worker with `AgentWorkflow` + `AgentActivities`
    /// registered.
    ///
    /// Panics if [`Self::llm`] was not called.
    pub fn build_worker(self, runtime: &CoreRuntime) -> Result<Worker, AgentError> {
        let llm = self
            .llm
            .expect("AgentWorkerBuilder::llm(...) must be called before build_worker()");
        let registry = self.tools.build();

        // Publish the tool catalog so the workflow can include it in each
        // llm_chat payload. The library does NOT inject any synthetic
        // tools — including question-asking. Users who want human-in-the-
        // loop register their own `ask_user`-style tool whose `execute()`
        // blocks until an answer is delivered (see the math_agent example).
        let catalog = registry.to_schemas();
        // OnceCell::set is fallible if already set; in long-running test
        // processes we tolerate re-initialization with the same data.
        let _ = WORKER_TOOL_CATALOG.set(catalog);

        let memory: Arc<dyn MemoryProvider> = self
            .memory
            .unwrap_or_else(|| Arc::new(SlidingWindowMemory::default()));
        let _ = WORKER_MEMORY.set(memory);

        let activities = AgentActivities::new(llm, registry);

        let opts = WorkerOptions::new(&self.queue)
            .register_workflow::<AgentWorkflow>()
            .register_activities(activities)
            .build();

        Worker::new(runtime, self.client, opts)
            .map_err(|e| AgentError::Other(format!("worker init: {e}")))
    }
}
