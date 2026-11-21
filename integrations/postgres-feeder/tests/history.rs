//! Tests that change server-wide settings or fill the WAL. Cargo runs test
//! binaries one after another, and the tests of this file hold one lock, so
//! nothing else runs against the server meanwhile.
//!
//! The server must run with `max_slot_wal_keep_size = 64MB`
//! (scripts/start-test-postgres.sh and the CI job do).

mod common;

use common::{Env, connect, preload, wait_for};
use std::time::Duration;
use tokio::sync::Mutex;

static SERIAL: Mutex<()> = Mutex::const_new(());

fn signal(pid: u32, sig: &str) {
    let status = std::process::Command::new("kill")
        .args([sig, &pid.to_string()])
        .status()
        .expect("kill");
    assert!(status.success());
}

/// C7, C1: the slot is invalidated while the feeder is stopped; the feeder
/// declares a gap (Knotrel desynchronized) and rebuilds.
#[tokio::test(flavor = "multi_thread")]
async fn slot_invalidated_declares_gap_and_rebuilds() {
    let _serial = SERIAL.lock().await;
    let Some(mut env) = Env::new("lostwal").await else {
        return;
    };
    let keep: String = env
        .admin
        .query_one("SELECT current_setting('max_slot_wal_keep_size')", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        keep, "64MB",
        "run the server with max_slot_wal_keep_size=64MB"
    );
    preload(&env, 200, 400).await;
    env.start_feeder(None);
    env.converge().await;

    let pid = env.feeder_pid();
    signal(pid, "-STOP");
    env.sql("INSERT INTO knotrel_graph.nodes VALUES (777777, true)")
        .await;
    env.sql("CREATE TABLE filler (id int, pad text)").await;
    // About 200 MB of WAL on a table outside the publication, then
    // checkpoints: the slot exceeds the 64 MB bound and is invalidated.
    for _ in 0..4 {
        env.sql(
            "INSERT INTO filler SELECT g, repeat('x', 1000) FROM generate_series(1, 50000) g; \
                 SELECT pg_switch_wal(); CHECKPOINT;",
        )
        .await;
    }
    wait_for("slot invalidation", Duration::from_secs(60), || async {
        env.slots()
            .await
            .iter()
            .any(|(_, _, status, _)| status.as_deref() == Some("lost"))
    })
    .await;
    signal(pid, "-CONT");
    env.converge().await;
    let log = env.log();
    assert!(log.contains("declared a source gap"), "no gap declared");
    let stats = env.stats().await;
    assert!(
        stats["generation"].as_u64().unwrap() >= 2,
        "rebuilt with a replace import"
    );
    env.sql("DROP TABLE filler").await;
    env.cleanup().await;
}

/// C8: unbounded retention is refused unless explicitly allowed.
#[tokio::test(flavor = "multi_thread")]
async fn unbounded_retention_is_refused() {
    let _serial = SERIAL.lock().await;
    let Some(env) = Env::new("unbounded").await else {
        return;
    };
    let root = connect(&common::admin_url().unwrap()).await;
    root.batch_execute("ALTER SYSTEM SET max_slot_wal_keep_size = -1")
        .await
        .unwrap();
    root.batch_execute("SELECT pg_reload_conf()").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let refused = env.feeder_command_output(&["check"], &[]).await;
    let allowed = env
        .feeder_command_output(
            &["check"],
            &[("KNOTREL_FEEDER_ALLOW_UNBOUNDED_WAL", "true")],
        )
        .await;
    root.batch_execute("ALTER SYSTEM SET max_slot_wal_keep_size = '64MB'")
        .await
        .unwrap();
    root.batch_execute("SELECT pg_reload_conf()").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(refused.0, 2, "{}", refused.1);
    assert!(
        refused.1.contains("max_slot_wal_keep_size is -1"),
        "{}",
        refused.1
    );
    assert_eq!(allowed.0, 0, "{}", allowed.1);
    env.cleanup().await;
}
