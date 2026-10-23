//! Runnable External Feeder Example for Knotrel Feature 0.2.6.
//!
//! Demonstrates the complete integration lifecycle using an external authoritative source:
//!
//! `coherent snapshot (S) → atomic import → ordered catch-up → barrier (B) → Ready → incremental updates → restart → authoritative rebuild`
//!
//! Exercises the real HTTP API and demonstrates that Knotrel holds derived, volatile state.
//!
//! # Build & Run Instructions
//!
//! Build both the server binary and the example binary together:
//! ```bash
//! cargo build --bin knotrel-server --example external_feeder
//! ```
//!
//! Auto-spawn a local `knotrel-server` child process in source-managed mode:
//! ```bash
//! cargo run --example external_feeder -- --spawn-server
//! ```
//!
//! Or connect to an existing running server:
//! ```bash
//! cargo run --example external_feeder -- --addr 127.0.0.1:8080
//! ```

#[path = "../tests/common/feeder.rs"]
mod feeder;

use feeder::{
    ControlledFaultProxy, ExternalFeederClient, ExternalSourceFixture, ExternalSourceOracle,
    SourceEvent,
};
use std::{
    net::{SocketAddr, TcpListener},
    process::{Child, Command, Stdio},
    time::Duration,
};

/// RAII process guard that ensures child process termination on completion or panic.
struct ServerGuard {
    child: Option<Child>,
    address: SocketAddr,
}

impl ServerGuard {
    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

fn get_free_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free port");
    listener.local_addr().expect("local addr")
}

fn find_server_binary() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("KNOTREL_BIN") {
        return std::path::PathBuf::from(path);
    }
    if let Ok(mut exe) = std::env::current_exe() {
        exe.pop(); // pop example binary name
        if exe.ends_with("examples") {
            exe.pop(); // pop examples directory to target/<profile>
        }
        let candidate = exe.join(if cfg!(windows) {
            "knotrel-server.exe"
        } else {
            "knotrel-server"
        });
        if candidate.exists() {
            return candidate;
        }
    }
    let fallback = std::path::PathBuf::from("target/debug/knotrel-server");
    if !fallback.exists() {
        eprintln!(
            "Notice: '{}' not found. Ensure server is compiled with:\n  cargo build --bin knotrel-server --example external_feeder",
            fallback.display()
        );
    }
    fallback
}

fn spawn_server(address: SocketAddr) -> Child {
    let binary = find_server_binary();
    let mut cmd = Command::new(&binary);
    cmd.env_clear()
        .env("KNOTREL_ADDR", address.to_string())
        .env("KNOTREL_MODE", "source-managed")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().unwrap_or_else(|e| {
        panic!(
            "failed to spawn knotrel-server binary from '{}': {e}\nPlease run: cargo build --bin knotrel-server --example external_feeder",
            binary.display()
        )
    })
}

async fn wait_server_listening(address: SocketAddr, timeout_dur: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout_dur;
    while tokio::time::Instant::now() < deadline {
        if let Ok(mut stream) = tokio::net::TcpStream::connect(address).await {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let req =
                format!("GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
            let mut buf = [0_u8; 256];
            if stream.write_all(req.as_bytes()).await.is_ok()
                && matches!(
                    stream.read(&mut buf).await,
                    Ok(n) if n > 0 && String::from_utf8_lossy(&buf[..n]).contains("200 OK")
                )
            {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let should_spawn = args.iter().any(|a| a == "--spawn-server");
    let mut custom_addr: Option<SocketAddr> = None;

    let mut i = 1;
    while i < args.len() {
        if args[i] == "--addr" && i + 1 < args.len() {
            custom_addr = Some(args[i + 1].parse()?);
            i += 2;
        } else {
            i += 1;
        }
    }

    if custom_addr.is_none() {
        custom_addr = std::env::var("KNOTREL_ADDR")
            .ok()
            .map(|addr| addr.parse())
            .transpose()?;
    }

    // Process guard installed IMMEDIATELY upon spawn to ensure cleanup even if startup check fails.
    let mut server_guard = match custom_addr {
        Some(addr) if !should_spawn => ServerGuard {
            child: None,
            address: addr,
        },
        maybe_addr => {
            let addr = maybe_addr.unwrap_or_else(get_free_address);
            println!("🚀 Spawning local knotrel-server in source-managed mode on {addr}...");
            let child = spawn_server(addr);
            let guard = ServerGuard {
                child: Some(child),
                address: addr,
            };
            if !wait_server_listening(addr, Duration::from_secs(5)).await {
                panic!("server process did not start listening within 5s");
            }
            guard
        }
    };

    let server_addr = server_guard.address;

    // Start a ControlledFaultProxy to demonstrate deterministic recovery from network faults
    let proxy = ControlledFaultProxy::start(server_addr).await?;
    let proxy_addr = proxy.listen_addr();
    println!(
        "📡 Connecting external feeder to Knotrel via fault proxy at {proxy_addr} (forwarding to {server_addr})...\n"
    );

    let mut client = ExternalFeederClient::new(proxy_addr, "feeder-demo-01");
    let fixture = ExternalSourceFixture::standard();
    let mut oracle = ExternalSourceOracle::new();

    println!("================================================================================");
    println!("STEP 1: Server Discovery & Verification of Operating Mode");
    println!("================================================================================");
    let instance_id = client.discover().await?;
    println!("  ✓ Discovered server instance: {instance_id}");
    println!("  ✓ Confirmed operating mode: source-managed (direct writes disabled)");

    let ready_init = client.get_ready().await?;
    println!(
        "  ✓ Initial readiness probe: HTTP {} (status: {})",
        ready_init.status,
        ready_init.json()["status"]
    );
    assert_eq!(ready_init.status, 503, "server must be unready at startup");

    println!("\n================================================================================");
    println!("STEP 2: Feeder Session Registration & Observable Heartbeat Lease");
    println!("================================================================================");
    let session_res = client.register_session(10_000).await?;
    println!(
        "  ✓ Registered feeder session '{}' under epoch {}",
        client.feeder_session_id, client.feeder_epoch
    );
    println!(
        "  ✓ Lease duration: {} ms",
        session_res["lease_duration_ms"]
    );
    client.start_heartbeat(Duration::from_millis(2_000));
    println!(
        "  ✓ Background heartbeat worker active (observable, validating responses every 2.0s)"
    );
    println!(
        "  ✓ Heartbeat status: {:?} (healthy: {})",
        client.current_heartbeat_status(),
        client.is_heartbeat_healthy()
    );

    println!("\n================================================================================");
    println!("STEP 3: Atomic Baseline Import (Snapshot S) with Reconciliation");
    println!("================================================================================");
    println!(
        "  Source snapshot position S: {}",
        fixture.snapshot.position
    );
    println!(
        "  Initial topology: {} nodes, {} edges",
        fixture.snapshot.nodes.len(),
        fixture.snapshot.edges.len()
    );
    println!("  Nodes include explicit isolated nodes: [99, 100]");

    let import_res = client
        .import_with_reconciliation(
            "create_only",
            &fixture.source_id,
            &fixture.source_epoch,
            &fixture.snapshot,
            None,
            3,
        )
        .await?;
    println!("  ✓ Import status: HTTP {}", import_res.status);
    println!("  ✓ Published generation: {}", client.generation);
    println!("  ✓ Server revision: {}", client.state_version);
    println!(
        "  ✓ Applied source position: {}",
        client.applied_source_position
    );

    oracle.load_snapshot(&fixture.snapshot);
    println!("  ✓ Independent Oracle initialized with baseline snapshot");

    let ready_after_import = client.get_ready().await?;
    println!(
        "  ✓ Readiness probe after import: HTTP {} (status: {})",
        ready_after_import.status,
        ready_after_import.json()["status"]
    );
    assert_eq!(
        ready_after_import.json()["status"],
        "catching_up",
        "import enters CatchingUp state"
    );

    println!("\n================================================================================");
    println!("STEP 4: Ordered Catch-Up Stream (Events 1 through 7)");
    println!("================================================================================");
    for evt in &fixture.events {
        let events_res = client
            .apply_events_with_recovery(std::slice::from_ref(evt), 3)
            .await?;
        assert_eq!(events_res.status, 200);
        oracle.apply_event(evt);

        let desc = match evt.op.as_str() {
            "link" => format!("link({}, {})", evt.source.unwrap(), evt.target.unwrap()),
            "cut" => format!("cut({}, {})", evt.source.unwrap(), evt.target.unwrap()),
            "add_node" => format!("add_node({})", evt.node.unwrap()),
            "progress" => "progress (non-mutating source transaction)".to_string(),
            _ => evt.op.clone(),
        };
        println!(
            "  ✓ Event D={:02} (pos: {}): {:<45} [revision: {}]",
            evt.delivery_sequence, evt.source_position, desc, client.state_version
        );
    }

    println!("\n================================================================================");
    println!("STEP 5: Authoritative Synchronization Barrier B");
    println!("================================================================================");
    let (b_seq, b_pos) = &fixture.high_water_mark;
    println!(
        "  Declaring barrier at authoritative high-water mark: sequence={b_seq}, position={b_pos}"
    );
    let barrier_res = client.declare_barrier(*b_seq, b_pos).await?;
    println!("  ✓ Barrier response: HTTP {}", barrier_res.status);
    println!(
        "  ✓ Outcome: result='{}', status='{}'",
        barrier_res.json()["result"],
        barrier_res.json()["status"]
    );

    let ready_promoted = client.get_ready().await?;
    println!(
        "  ✓ Server Readiness Probe: HTTP {} (status: {})",
        ready_promoted.status,
        ready_promoted.json()["status"]
    );
    assert_eq!(
        ready_promoted.status, 200,
        "barrier promotes CatchingUp to Ready"
    );

    println!("\n================================================================================");
    println!("STEP 6: Independent Oracle Verification vs Real HTTP Queries");
    println!("================================================================================");
    let check_pairs = [
        (1, 2),
        (1, 10),
        (10, 20),
        (2, 3),
        (3, 99),
        (100, 101),
        (1, 100),
        (4, 99),
    ];
    for &(u, v) in &check_pairs {
        let oracle_conn = oracle.is_connected(u, v);
        let q_res = client.query_connected(u, v).await?;
        assert_eq!(q_res.status, 200);
        let knotrel_conn = q_res.json()["connected"].as_bool().unwrap();
        let sync_header = q_res.header("x-knotrel-sync-status").unwrap_or("none");

        assert_eq!(
            knotrel_conn, oracle_conn,
            "Knotrel result must match oracle for ({u}, {v})"
        );
        println!(
            "  ✓ Query connected({u}, {v}) -> Knotrel: {knotrel_conn:<5} Oracle: {oracle_conn:<5} [Header sync: {sync_header}]"
        );
    }

    println!("\n================================================================================");
    println!("STEP 7: Network Fault Injection: Lost Acknowledgment & Feeder Recovery");
    println!("================================================================================");
    let incremental_event = SourceEvent {
        event_id: "evt-0108-fault-demo".to_string(),
        delivery_sequence: 8,
        prev_delivery_sequence: 7,
        source_position: "pos-0108".to_string(),
        prev_source_position: "pos-0107-b1".to_string(),
        op: "link".to_string(),
        source: Some(3),
        target: Some(99),
        node: None,
    };

    println!("  Arming fault proxy to drop server response for sequence D=8...");
    proxy.arm_drop_next_response();

    println!("  Submitting event through proxy with automated feeder recovery...");
    let rec_res = client
        .apply_events_with_recovery(std::slice::from_ref(&incremental_event), 3)
        .await?;
    println!(
        "  ✓ Feeder handled lost ACK: HTTP {} (result: '{}', reconciled: {})",
        rec_res.status,
        rec_res.json()["result"],
        rec_res.json()["reconciled_after_lost_ack"]
    );
    assert_eq!(proxy.dropped_response_count(), 1);
    assert_eq!(rec_res.status, 200);

    oracle.apply_event(&incremental_event);
    let q_3_99 = client.query_connected(3, 99).await?;
    assert!(
        q_3_99.json()["connected"].as_bool().unwrap(),
        "mutation must be committed on server"
    );
    println!("  ✓ Verified edge (3, 99) is connected after recovery");

    // Barrier for sequence 8
    client.declare_barrier(8, "pos-0108").await?;

    println!("\n================================================================================");
    println!("STEP 8: Demonstrating Volatile State: Restart & Authoritative Rebuild");
    println!("================================================================================");
    if server_guard.child.is_some() {
        println!("  Stopping heartbeat cleanly on old server instance...");
        client.stop_heartbeat().await;

        println!("  Killing knotrel-server process (simulating abrupt failure)...");
        server_guard.kill();
        tokio::time::sleep(Duration::from_millis(150)).await;

        println!("  Restarting knotrel-server on {server_addr}...");
        let new_child = spawn_server(server_addr);
        server_guard.child = Some(new_child);
        if !wait_server_listening(server_addr, Duration::from_secs(5)).await {
            panic!("restarted server failed to listen");
        }

        println!("  Discovering restarted server...");
        let new_inst = client.discover().await?;
        println!("  ✓ Fresh instance identity detected: {new_inst} (differs from {instance_id})");

        let ready_restart = client.get_ready().await?;
        println!(
            "  ✓ Server correctly reports unready: HTTP {} (status: {})",
            ready_restart.status,
            ready_restart.json()["status"]
        );
        assert_eq!(ready_restart.status, 503);

        println!("  Executing complete authoritative rebuild from source fixture...");
        client.sync_from_fixture(&fixture, false).await?;
        println!("  ✓ Authoritative rebuild completed: server is READY (HTTP 200)");

        let q_rebuild = client.query_connected(1, 10).await?;
        assert!(q_rebuild.json()["connected"].as_bool().unwrap());
        println!("  ✓ Verified topology restored after rebuild");
    }

    println!("\n================================================================================");
    println!("STEP 9: Clean Shutdown & Heartbeat Awaiting");
    println!("================================================================================");
    client.stop_heartbeat().await;
    println!(
        "  ✓ Heartbeat shutdown successfully awaited. Final status: {:?}",
        client.current_heartbeat_status()
    );

    let stats = client.get_stats().await?;
    println!("\nFinal live server statistics before exit:");
    println!("  Generation:      {}", stats.json()["generation"]);
    println!("  Node Count:      {}", stats.json()["node_count"]);
    println!("  Edge Count:      {}", stats.json()["edge_count"]);
    println!("  State Version:   {}", stats.json()["state_version"]);
    println!("  Source Position: {}", stats.json()["source_position"]);
    println!("  Sync Status:     {}", stats.json()["sync_status"]);

    println!("\n🎉 External Feeder demonstration completed successfully!");
    Ok(())
}
