//! Example A: Basic dynamic connectivity operations using Knotrel.
//!
//! Demonstrates:
//! - Initializing a graph with the default engine.
//! - Adding isolated nodes and linking edges.
//! - Querying reachability between nodes.
//! - Cutting edges (bridge cuts) and observing disconnected components.
//! - Re-linking edges to restore connectivity.
//!
//! Run with: `cargo run -p knotrel-core --example basic`

use knotrel_core::{ConnectivityGraph, EngineConfig};
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("=== Knotrel Basic Dynamic Connectivity Example ===\n");

    // 1. Create a dynamic connectivity graph.
    // EngineConfig::CompactBfs is the default general-purpose engine.
    let mut graph = ConnectivityGraph::new(EngineConfig::CompactBfs);
    println!("Created graph with engine: {}", graph.engine().as_str());

    // 2. Add vertices explicitly (optional; `link` implicitly creates endpoints).
    graph.add_node(1)?;
    graph.add_node(2)?;
    graph.add_node(3)?;
    graph.add_node(4)?;
    println!(
        "Registered 4 nodes. Current node count: {}",
        graph.node_count()
    );

    // 3. Link edges to form two disconnected paths: 1-2 and 3-4.
    graph.link(1, 2)?;
    graph.link(3, 4)?;
    println!(
        "Linked (1, 2) and (3, 4). Edge count: {}",
        graph.edge_count()
    );

    // 4. Query connectivity.
    println!("Is 1 connected to 2? {}", graph.connected(1, 2)?);
    println!("Is 3 connected to 4? {}", graph.connected(3, 4)?);
    println!("Is 1 connected to 4? {}", graph.connected(1, 4)?);
    assert!(graph.connected(1, 2)?);
    assert!(graph.connected(3, 4)?);
    assert!(!graph.connected(1, 4)?);

    // 5. Connect the two components with a bridge edge (2, 3).
    println!("\nAdding bridge edge (2, 3)...");
    graph.link(2, 3)?;
    println!("Is 1 connected to 4 now? {}", graph.connected(1, 4)?);
    assert!(graph.connected(1, 4)?);

    // 6. Cut the bridge edge (2, 3).
    println!("\nCutting bridge edge (2, 3)...");
    let was_removed = graph.cut(2, 3)?;
    println!("Edge (2, 3) removed: {was_removed}");
    println!("Is 1 connected to 4 after cut? {}", graph.connected(1, 4)?);
    assert!(!graph.connected(1, 4)?);

    // Vertices remain alive and registered after cuts.
    println!("Node count after cut (retained): {}", graph.node_count());
    assert_eq!(graph.node_count(), 4);

    // 7. Relink a different edge (1, 4) to restore connectivity directly.
    println!("\nRelinking via alternative edge (1, 4)...");
    graph.link(1, 4)?;
    println!("Is 1 connected to 3 now? {}", graph.connected(1, 3)?);
    assert!(graph.connected(1, 3)?);

    println!("\nBasic dynamic connectivity example completed successfully.");
    Ok(())
}
