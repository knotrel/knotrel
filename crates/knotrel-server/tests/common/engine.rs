//! Engine under test for suites that run on more than one engine.
//!
//! `KNOTREL_TEST_ENGINE` selects it (`compact-bfs` when unset). CI runs the
//! lifecycle, import, feeder-event, batch and bootstrap suites a second time
//! with `cluster-forest` (ADR 0014, CF4). An unknown value fails loudly.

use knotrel_core::EngineConfig;

/// Returns the engine selected by `KNOTREL_TEST_ENGINE`.
pub fn engine() -> EngineConfig {
    match std::env::var("KNOTREL_TEST_ENGINE").as_deref() {
        Err(_) | Ok("compact-bfs") => EngineConfig::CompactBfs,
        Ok("cluster-forest") => EngineConfig::ClusterForest,
        Ok(other) => panic!("KNOTREL_TEST_ENGINE={other:?} is not a lifecycle engine"),
    }
}
