//! Tool registry — bridges AutoAgents' [`ToolT`] trait into a name-based
//! dispatch table used by the `execute_tool` activity.
//!
//! Tools authored with AutoAgents' `#[tool]` derive can be registered as-is.
//! The workflow only references tools by name; actual `ToolT::execute` calls
//! happen exclusively inside the activity, preserving workflow determinism.

use std::collections::HashMap;
use std::sync::Arc;

use autoagents_core::tool::ToolT;

use crate::state::ToolSchema;

/// Immutable map of tool name → boxed tool implementation.
///
/// Shared across all in-flight activity invocations on a worker via `Arc`.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Arc<HashMap<String, Arc<dyn ToolT>>>,
}

impl ToolRegistry {
    /// Start building a registry with [`ToolRegistryBuilder`].
    pub fn builder() -> ToolRegistryBuilder {
        ToolRegistryBuilder::default()
    }

    /// Look up a registered tool by its name. Returns `None` if no tool with
    /// that name was registered with the worker.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn ToolT>> {
        self.tools.get(name)
    }

    /// Iterate the names of every registered tool (no defined order).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    /// Produce JSON-Schema descriptions of every registered tool to send to
    /// the LLM each turn.
    pub fn to_schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .values()
            .map(|t| ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                args_schema: t.args_schema(),
            })
            .collect()
    }
}

/// Fluent builder for [`ToolRegistry`]. Use via [`ToolRegistry::builder`].
#[derive(Default)]
pub struct ToolRegistryBuilder {
    tools: HashMap<String, Arc<dyn ToolT>>,
}

impl ToolRegistryBuilder {
    /// Register a tool. Tools are keyed by [`ToolT::name`]; later
    /// registrations with the same name overwrite earlier ones.
    #[allow(clippy::should_implement_trait)]
    #[must_use]
    pub fn add(mut self, tool: Arc<dyn ToolT>) -> Self {
        self.tools.insert(tool.name().to_string(), tool);
        self
    }

    /// Freeze the builder into an immutable [`ToolRegistry`].
    pub fn build(self) -> ToolRegistry {
        ToolRegistry {
            tools: Arc::new(self.tools),
        }
    }
}
