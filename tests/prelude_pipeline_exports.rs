//! Compile-only assertions for the pipeline / optim re-exports in the prelude.
//!
//! Removing or renaming any of the listed items from `temporal_agent_rs::prelude`
//! will break this test crate, surfacing the change as an API break.
//!
//! Retry types (`RetryLayer`, `RetryConfig`, `default_is_retryable`) are
//! intentionally NOT imported. Temporal activity `RetryPolicy` owns retry
//! semantics for this crate; the absence of retry types in the prelude is
//! load-bearing. Do not add them here.

#[allow(dead_code, unused_imports, unused_variables)]
fn _prelude_pipeline_exports_compile() {
    use temporal_agent_rs::prelude::{
        CacheConfig, CacheLayer, ChatCacheKeyMode, FallbackConfig, FallbackLayer, PipelineBuilder,
        default_is_fallbackable,
    };

    let _: CacheConfig = CacheConfig::default();
    let _: CacheLayer = CacheLayer::with_defaults();
    let _: ChatCacheKeyMode = ChatCacheKeyMode::UserPromptOnly;
    let _: FallbackConfig = FallbackConfig {
        fallbackable: default_is_fallbackable,
    };

    // `PipelineBuilder::new` requires an `Arc<dyn LLMProvider>`. Discarding
    // the constructor as a function item type-checks the re-export without
    // needing a real provider.
    let _ = PipelineBuilder::new;
}
