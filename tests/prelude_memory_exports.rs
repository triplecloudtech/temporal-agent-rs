//! Compile-only assertions for the memory-provider re-exports in the prelude.
//!
//! Removing or renaming any of the listed items from `temporal_agent_rs::prelude`
//! will break this test crate, surfacing the change as an API break.

#[allow(dead_code, unused_imports, unused_variables)]
fn _prelude_memory_exports_compile() {
    use std::sync::Arc;
    use temporal_agent_rs::prelude::{MemoryProvider, SlidingWindowMemory};

    let _: SlidingWindowMemory = SlidingWindowMemory::default();
    let _: Arc<dyn MemoryProvider> = Arc::new(SlidingWindowMemory::default());
}
