//! Example B: Bulk initialization and error handling using Knotrel.
//!
//! Demonstrates:
//! - Bulk graph construction via `ConnectivityGraph::try_from_edges`.
//! - Using realistic network topology data.
//! - Running exact reachability queries on the imported graph.
//! - Continuing incremental mutations after bulk import.
//! - Handling untrusted or invalid input gracefully without `unwrap()`.
//!
//! Run with: `cargo run -p knotrel-core --example bulk_import`

use knotrel_core::{ConnectivityGraph, EngineConfig, GraphError, GraphLimits, NodeId};
use std::error::Error;

fn main() -> Result<(), Box<dyn Error>> {
    println!("=== Knotrel Bulk Initialization Example ===\n");

    // 1. Define realistic topology data:
    // A network with two clusters connected via a gateway router:
    // Cluster A: Web Server (101), App Server (102), DB Server (103)
    // Gateway: (100)
    // Cluster B: Cache Node (201), Worker Node (202)
    // Isolated Standby Node: (999)
    let explicit_nodes: [NodeId; 1] = [999]; // Isolated node
    let initial_edges: [(NodeId, NodeId); 6] = [
        (100, 101), // Gateway <-> Web
        (101, 102), // Web <-> App
        (102, 103), // App <-> DB
        (100, 201), // Gateway <-> Cache
        (201, 202), // Cache <-> Worker
        (101, 103), // Redundant internal link Web <-> DB
    ];

    // Configure optional limits (e.g., max 1000 nodes, 5000 edges)
    let limits = GraphLimits {
        max_nodes: Some(1000),
        max_edges: Some(5000),
    };

    println!("Constructing graph via try_from_edges...");
    let mut graph = match ConnectivityGraph::try_from_edges(
        EngineConfig::CompactBfs,
        limits,
        &explicit_nodes,
        &initial_edges,
    ) {
        Ok(g) => {
            println!(
                "Successfully imported graph: {} nodes, {} edges",
                g.node_count(),
                g.edge_count()
            );
            g
        }
        Err(err) => {
            eprintln!("Failed to import graph: {err}");
            return Err(err.into());
        }
    };

    // 2. Query connectivity on the imported topology
    println!("\nQuerying connectivity on imported graph:");
    println!(
        "Is App Server (102) connected to Worker (202)? {}",
        graph.connected(102, 202)?
    );
    assert!(graph.connected(102, 202)?);

    println!(
        "Is Web Server (101) connected to Standby Node (999)? {}",
        graph.connected(101, 999)?
    );
    assert!(!graph.connected(101, 999)?);

    // 3. Continue incremental mutations on the loaded graph
    println!("\nIncrementally commissioning Standby Node (999)...");
    graph.link(999, 100)?;
    println!(
        "Is Standby Node (999) now connected to DB Server (103)? {}",
        graph.connected(999, 103)?
    );
    assert!(graph.connected(999, 103)?);

    // 4. Robust error handling on untrusted user-provided inputs
    println!("\nDemonstrating robust error handling without unwrap():");

    // Case A: Querying an unknown/unregistered vertex
    let unknown_node = 8888;
    match graph.connected(101, unknown_node) {
        Ok(connected) => {
            panic!("expected UnknownNode error, but connected returned {connected}");
        }
        Err(GraphError::UnknownNode { node }) => {
            println!("Gracefully handled: query referenced unknown node {node}");
        }
        Err(other) => return Err(other.into()),
    }

    // Case B: Rejecting self-loops without mutating graph state
    let self_loop_node = 101;
    match graph.link(self_loop_node, self_loop_node) {
        Ok(_) => panic!("expected SelfLoop error, but link succeeded"),
        Err(GraphError::SelfLoop { node }) => {
            println!("Gracefully handled: rejected self-loop on node {node}");
        }
        Err(other) => return Err(other.into()),
    }

    // Case C: Enforcing cardinality limits during bulk import
    let strict_limits = GraphLimits {
        max_nodes: Some(3),
        max_edges: Some(3),
    };
    let oversized_nodes = [1, 2, 3, 4, 5];
    let oversized_edges = [(1, 2), (2, 3), (3, 4)];

    match ConnectivityGraph::try_from_edges(
        EngineConfig::CompactBfs,
        strict_limits,
        &oversized_nodes,
        &oversized_edges,
    ) {
        Ok(_) => panic!("expected NodeLimitExceeded error, but bulk import succeeded"),
        Err(GraphError::NodeLimitExceeded { limit }) => {
            println!(
                "Gracefully handled: bulk import exceeded node limit {limit} (atomic rejection)"
            );
        }
        Err(other) => return Err(other.into()),
    }

    println!("\nBulk initialization example completed successfully.");
    Ok(())
}
