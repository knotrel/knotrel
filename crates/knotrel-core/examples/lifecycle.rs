//! Example D: node activation, edge enabling and node removal (v0.5.0).
//!
//! Demonstrates:
//! - Deactivating a node while keeping its registered edges.
//! - Changing an edge flag while an endpoint is inactive.
//! - Reactivating without overwriting edge flags.
//! - Permanent removal and re-creating an ID without its old edges.
//! - Registered versus active, enabled and effective counts.
//!
//! Run with: `cargo run -p knotrel-core --example lifecycle`

use knotrel_core::{EngineConfig, LifecycleGraph};
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("=== Knotrel Node and Edge Lifecycle Example ===\n");
    let mut graph = LifecycleGraph::new(EngineConfig::CompactBfs)?;
    // A path 1 - 2 - 3 plus a spur 2 - 4.
    graph.link(1, 2)?;
    graph.link(2, 3)?;
    graph.link(2, 4)?;

    graph.set_node_active(2, false)?;
    println!(
        "node 2 inactive: connected(1, 3) = {}",
        graph.connected(1, 3)?
    );
    graph.set_edge_enabled(2, 4, false)?;
    graph.set_node_active(2, true)?;
    println!(
        "node 2 active again: connected(1, 3) = {}",
        graph.connected(1, 3)?
    );
    println!(
        "edge 2-4 stayed disabled: {:?}, connected(1, 4) = {}",
        graph.edge_state(2, 4),
        graph.connected(1, 4)?
    );
    println!(
        "counts: {} registered nodes, {} active; {} registered edges, {} enabled, {} effective",
        graph.node_count(),
        graph.active_node_count(),
        graph.edge_count(),
        graph.enabled_edge_count(),
        graph.effective_edge_count()
    );

    graph.remove_node(2);
    graph.add_node(2)?;
    println!(
        "node 2 removed and re-created: edge 1-2 = {:?}, connected(1, 2) = {}",
        graph.edge_state(1, 2),
        graph.connected(1, 2)?
    );
    assert_eq!(graph.edge_count(), 0);
    Ok(())
}
