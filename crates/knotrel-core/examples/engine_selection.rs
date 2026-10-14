//! Example C: Engine selection and equivalence demonstration.
//!
//! Demonstrates:
//! - Selecting between BFS, Euler Tour Trees (ETT), and HDT via `EngineConfig`.
//! - Applying the identical sequence of mutations and queries to all three engines.
//! - Verifying that all three engines produce identical observable reachability results.
//!
//! Run with: `cargo run -p knotrel-core --example engine_selection`

use knotrel_core::{ConnectivityGraph, EngineConfig, NodeId};
use std::error::Error;

/// An operation to perform on a connectivity graph.
enum Action {
    AddNode(NodeId),
    Link(NodeId, NodeId),
    Cut(NodeId, NodeId),
    CheckConnected(NodeId, NodeId, bool),
}

fn exercise_engine(config: EngineConfig) -> Result<(), Box<dyn Error>> {
    println!(
        "--- Testing Engine: {:?} (as_str: '{}', experimental: {}) ---",
        config,
        config.as_str(),
        config.is_experimental()
    );

    let mut graph = ConnectivityGraph::new(config);
    assert_eq!(graph.engine(), config);

    // Sequence of operations testing dynamic connectivity:
    // 1. Create a cycle: 1 - 2 - 3 - 1
    // 2. Attach a tail: 3 - 4 - 5
    // 3. Create a disconnected component: 6 - 7
    // 4. Cut a cycle edge (1, 2) -> 1, 2, 3 should still be connected via (1, 3) and (3, 2)
    // 5. Cut a bridge edge (3, 4) -> 1, 2, 3 should become disconnected from 4, 5
    // 6. Cut tail edge (4, 5) -> 4 and 5 separate
    // 7. Relink (1, 5) -> 1, 2, 3 reconnect to 5
    let actions = [
        Action::AddNode(1),
        Action::AddNode(2),
        Action::AddNode(3),
        Action::AddNode(4),
        Action::AddNode(5),
        Action::AddNode(6),
        Action::AddNode(7),
        // Build cycle 1-2-3
        Action::Link(1, 2),
        Action::Link(2, 3),
        Action::Link(3, 1),
        // Attach tail 3-4-5
        Action::Link(3, 4),
        Action::Link(4, 5),
        // Disconnected component 6-7
        Action::Link(6, 7),
        // Initial verification
        Action::CheckConnected(1, 2, true),
        Action::CheckConnected(1, 5, true),
        Action::CheckConnected(1, 6, false),
        Action::CheckConnected(6, 7, true),
        // Cut cycle edge: alternative path 1-3-2 exists
        Action::Cut(1, 2),
        Action::CheckConnected(1, 2, true),
        // Cut bridge edge: disconnects {1, 2, 3} from {4, 5}
        Action::Cut(3, 4),
        Action::CheckConnected(1, 4, false),
        Action::CheckConnected(1, 5, false),
        Action::CheckConnected(4, 5, true),
        // Cut tail edge: separates 4 and 5
        Action::Cut(4, 5),
        Action::CheckConnected(4, 5, false),
        // Relink bridge directly from 1 to 5
        Action::Link(1, 5),
        Action::CheckConnected(2, 5, true),
        Action::CheckConnected(1, 5, true),
        Action::CheckConnected(4, 5, false),
    ];

    for action in actions {
        match action {
            Action::AddNode(node) => {
                graph.add_node(node)?;
            }
            Action::Link(u, v) => {
                graph.link(u, v)?;
            }
            Action::Cut(u, v) => {
                graph.cut(u, v)?;
            }
            Action::CheckConnected(u, v, expected) => {
                let actual = graph.connected(u, v)?;
                assert_eq!(
                    actual, expected,
                    "Engine {:?} mismatch for connected({}, {}): expected {}, got {}",
                    config, u, v, expected, actual
                );
            }
        }
    }

    println!(
        "Engine {:?} passed all equivalence checks (nodes: {}, edges: {}).\n",
        config,
        graph.node_count(),
        graph.edge_count()
    );

    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    println!("=== Knotrel Engine Selection & Equivalence Example ===\n");

    let engines = [
        EngineConfig::CompactBfs,
        EngineConfig::EulerTour,
        EngineConfig::Hdt,
    ];

    for &engine in &engines {
        exercise_engine(engine)?;
    }

    println!("All three engines produced identical, exact connectivity results.");
    Ok(())
}
