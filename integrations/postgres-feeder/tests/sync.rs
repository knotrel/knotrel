//! Synchronization, failure and restart tests against a real PostgreSQL
//! server (design document, sections 11 and 12).

mod common;

use common::{Env, Writers, preload, wait_for};
use std::time::Duration;

/// C1, C2: writes before, during and after the snapshot and the import.
#[tokio::test(flavor = "multi_thread")]
async fn initial_load_with_concurrent_writers() {
    let Some(mut env) = Env::new("initial").await else {
        return;
    };
    preload(&env, 3000, 6000).await;
    let writers = Writers::start(&env, 4, 3200, 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    env.start_feeder(None);
    tokio::time::sleep(Duration::from_secs(4)).await;
    let commits = writers.stop().await;
    assert!(commits > 50, "writers made too few commits: {commits}");
    env.converge().await;
    let stats = env.stats().await;
    assert_eq!(stats["generation"], 1, "one import, then streaming only");
    assert_eq!(env.slots().await.len(), 1);
    env.cleanup().await;
}

/// C1, C3: aborted transactions and savepoints never reach Knotrel; a large
/// transaction spans many batches.
#[tokio::test(flavor = "multi_thread")]
async fn rolled_back_and_mixed_transactions() {
    let Some(mut env) = Env::new("rollback").await else {
        return;
    };
    env.extra_env
        .push(("KNOTREL_FEEDER_MAX_BATCH".into(), "37".into()));
    preload(&env, 200, 300).await;
    env.start_feeder(None);
    env.converge().await;

    env.sql(
        "BEGIN; INSERT INTO knotrel_graph.nodes VALUES (900001, true), (900002, false); \
             INSERT INTO knotrel_graph.edges VALUES (900001, 900002, true); ROLLBACK;",
    )
    .await;
    env.sql("BEGIN; INSERT INTO knotrel_graph.nodes VALUES (900003, true); \
             SAVEPOINT s; INSERT INTO knotrel_graph.nodes VALUES (900004, true); ROLLBACK TO SAVEPOINT s; \
             INSERT INTO knotrel_graph.nodes VALUES (900005, false); COMMIT;")
        .await;
    // One transaction with about 3000 changes, sent in batches of 37.
    env.sql("BEGIN; \
             INSERT INTO knotrel_graph.nodes SELECT g, g % 3 <> 0 FROM generate_series(10000, 10999) g; \
             INSERT INTO knotrel_graph.edges SELECT g, g + 1, g % 4 <> 0 FROM generate_series(10000, 10998) g; \
             UPDATE knotrel_graph.nodes SET active = NOT active WHERE id BETWEEN 10000 AND 10499; \
             DELETE FROM knotrel_graph.nodes WHERE id BETWEEN 10900 AND 10950; \
             COMMIT;")
        .await;
    env.converge().await;
    let (nodes, _) = common::knotrel_topology(&env.server.url()).await;
    for absent in [900001, 900002, 900004] {
        assert!(
            !nodes.contains_key(&absent),
            "rolled-back node {absent} reached Knotrel"
        );
    }
    assert_eq!(nodes.get(&900005), Some(&false));
    env.cleanup().await;
}

/// C1: steady streaming with lifecycle changes, cascades and key updates.
#[tokio::test(flavor = "multi_thread")]
async fn steady_streaming_with_concurrent_writers() {
    let Some(mut env) = Env::new("steady").await else {
        return;
    };
    preload(&env, 400, 800).await;
    env.start_feeder(None);
    env.converge().await;
    let writers = Writers::start(&env, 6, 450, 7).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    writers.stop().await;
    env.converge().await;
    // Deferred constraint style: an edge before its nodes, then an inactive node.
    env.sql("BEGIN; \
             ALTER TABLE knotrel_graph.edges ALTER CONSTRAINT edges_source_fkey DEFERRABLE INITIALLY DEFERRED; \
             ALTER TABLE knotrel_graph.edges ALTER CONSTRAINT edges_target_fkey DEFERRABLE INITIALLY DEFERRED; \
             COMMIT; \
             BEGIN; INSERT INTO knotrel_graph.edges VALUES (700001, 700002, true); \
             INSERT INTO knotrel_graph.nodes VALUES (700001, false), (700002, true); COMMIT;")
        .await;
    env.converge().await;
    env.cleanup().await;
}

/// C1, C4: a feeder crash at each critical point, then a restart.
#[tokio::test(flavor = "multi_thread")]
async fn feeder_crash_at_each_fault_point() {
    let Some(mut env) = Env::new("crash").await else {
        return;
    };
    env.extra_env
        .push(("KNOTREL_FEEDER_MAX_BATCH".into(), "20".into()));
    preload(&env, 300, 600).await;
    for (round, point) in [
        "after_slot_created",
        "after_snapshot_read",
        "after_import",
        "before_events:3",
        "after_events_applied:2",
        "after_ack:2",
    ]
    .into_iter()
    .enumerate()
    {
        if point.starts_with("after_s") || point == "after_import" {
            // Bootstrap points: a new Knotrel instance forces a bootstrap.
            env.restart_server().await;
        }
        let writers = Writers::start(&env, 3, 350, 100 + round as u64).await;
        env.start_feeder(Some(point));
        let code = env.wait_feeder(Duration::from_secs(60)).await;
        writers.stop().await;
        assert_eq!(code, None, "{point}: the feeder must abort, not exit");
        assert!(
            env.log().contains("fault injection: aborting"),
            "{point}: fault not reached"
        );
        assert_confirmed_within_frontier(&env).await;
        env.start_feeder(None);
        env.converge().await;
        env.kill_feeder().await;
    }
    // Mid-transaction crash: a 500-change transaction, crash after its third batch.
    env.start_feeder(None);
    env.converge().await;
    env.kill_feeder().await;
    env.sql("INSERT INTO knotrel_graph.nodes SELECT g, true FROM generate_series(20000, 20499) g")
        .await;
    env.start_feeder(Some("after_events_applied:3"));
    assert_eq!(env.wait_feeder(Duration::from_secs(60)).await, None);
    let position = env.stats().await["source_position"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        !position.ends_with(";end"),
        "expected a crash inside the transaction, at {position}"
    );
    assert_confirmed_within_frontier(&env).await;
    env.start_feeder(None);
    env.converge().await;
    wait_for("one feeder slot", Duration::from_secs(10), || async {
        env.slots().await.len() == 1
    })
    .await;
    env.cleanup().await;
}

/// Rule R1 as observed after a crash: if Knotrel stopped inside a transaction
/// with commit LSN C, the slot has not confirmed past C.
async fn assert_confirmed_within_frontier(env: &Env) {
    let stats = env.stats().await;
    let Some(position) = stats["source_position"].as_str() else {
        return;
    };
    if position.ends_with(";end") {
        return;
    }
    let lsn = position
        .split(';')
        .find_map(|f| f.strip_prefix("lsn="))
        .expect("position has an LSN");
    let slot = position
        .split(';')
        .find_map(|f| f.strip_prefix("slot="))
        .expect("position has a slot");
    let ok: bool = env
        .admin
        .query_one(
            "SELECT pg_wal_lsn_diff(confirmed_flush_lsn, $1::text::pg_lsn) <= 0 \
             FROM pg_replication_slots WHERE slot_name = $2",
            &[&lsn, &slot],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        ok,
        "slot {slot} confirmed past the partly applied transaction at {lsn}"
    );
}

/// C1, C5: the replication connection is terminated during streaming, and
/// Knotrel answers are lost.
#[tokio::test(flavor = "multi_thread")]
async fn disconnections_and_lost_answers() {
    let Some(mut env) = Env::new("disconnect").await else {
        return;
    };
    preload(&env, 300, 600).await;
    env.start_feeder(Some("lose_events_response:3"));
    env.converge().await;
    let writers = Writers::start(&env, 3, 350, 21).await;
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(800)).await;
        env.sql(&format!(
            "SELECT pg_terminate_backend(active_pid) FROM pg_replication_slots \
             WHERE left(slot_name::text, length('{p}_')) = '{p}_' AND active_pid IS NOT NULL",
            p = env.prefix
        ))
        .await;
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    writers.stop().await;
    env.converge().await;
    let log = env.log();
    assert!(
        log.contains("discarding the answer"),
        "lost answer not injected"
    );
    assert!(log.contains("reconciling"), "lost answer not reconciled");
    assert!(log.contains("reconnecting"), "disconnection not observed");
    assert_eq!(env.stats().await["generation"], 1, "no rebuild needed");
    env.cleanup().await;
}

/// C1, C6: Knotrel is killed and restarted; the feeder rebuilds on the new
/// instance.
#[tokio::test(flavor = "multi_thread")]
async fn knotrel_restart_rebuilds() {
    let Some(mut env) = Env::new("restart").await else {
        return;
    };
    preload(&env, 500, 1000).await;
    env.start_feeder(None);
    env.converge().await;
    let old_instance = env.stats().await["instance_id"].clone();
    let writers = Writers::start(&env, 3, 550, 31).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    env.restart_server().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    writers.stop().await;
    env.converge().await;
    let stats = env.stats().await;
    assert_ne!(stats["instance_id"], old_instance);
    assert_eq!(
        stats["generation"], 1,
        "the new instance got exactly one create_only import"
    );
    assert!(env.log().contains("rebuilding on the new instance"));
    wait_for("old slot cleanup", Duration::from_secs(10), || async {
        env.slots().await.len() == 1
    })
    .await;
    env.cleanup().await;
}

/// A bootstrap that takes longer than the lease (a large image) keeps its
/// session: the import is accepted the first time.
#[tokio::test(flavor = "multi_thread")]
async fn slow_bootstrap_keeps_the_lease() {
    let Some(mut env) = Env::new("slowboot").await else {
        return;
    };
    preload(&env, 200, 400).await;
    env.start_feeder(Some("slow_bootstrap"));
    env.converge().await;
    let log = env.log();
    assert!(log.contains("fault injection: pausing"), "the pause ran");
    assert!(
        !log.contains("import refused"),
        "the import was refused after the pause:\n{log}"
    );
    assert_eq!(env.stats().await["generation"], 1);
    env.cleanup().await;
}

/// C1: TRUNCATE during streaming causes a rebuild.
#[tokio::test(flavor = "multi_thread")]
async fn truncate_rebuilds() {
    let Some(mut env) = Env::new("truncate").await else {
        return;
    };
    preload(&env, 300, 600).await;
    env.start_feeder(None);
    env.converge().await;
    env.sql(
        "TRUNCATE knotrel_graph.nodes CASCADE; \
             INSERT INTO knotrel_graph.nodes SELECT g, true FROM generate_series(1, 50) g; \
             INSERT INTO knotrel_graph.edges SELECT g, g + 1, true FROM generate_series(1, 49) g;",
    )
    .await;
    env.converge().await;
    let stats = env.stats().await;
    assert!(
        stats["generation"].as_u64().unwrap() >= 2,
        "a replace import happened"
    );
    assert!(env.log().contains("truncated"));
    env.cleanup().await;
}

/// C8: a foreign slot survives rebuilds; the feeder keeps one slot.
#[tokio::test(flavor = "multi_thread")]
async fn foreign_slots_untouched_and_slot_count_bounded() {
    let Some(mut env) = Env::new("slots").await else {
        return;
    };
    let foreign = format!("other_{}", env.prefix);
    env.sql(&format!(
        "SELECT pg_create_logical_replication_slot('{foreign}', 'pgoutput')"
    ))
    .await;
    preload(&env, 100, 150).await;
    env.start_feeder(None);
    env.converge().await;
    for _ in 0..2 {
        env.restart_server().await;
        env.converge().await;
    }
    wait_for("one feeder slot", Duration::from_secs(10), || async {
        env.slots().await.len() == 1
    })
    .await;
    let foreign_exists: bool = env
        .admin
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_replication_slots WHERE slot_name = $1)",
            &[&foreign],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        foreign_exists,
        "a slot without the feeder prefix was dropped"
    );
    env.sql(&format!("SELECT pg_drop_replication_slot('{foreign}')"))
        .await;

    // drop-slots removes only inactive feeder slots.
    env.kill_feeder().await;
    wait_for("slot inactive", Duration::from_secs(10), || async {
        env.slots().await.iter().all(|(_, active, _, _)| !active)
    })
    .await;
    let (code, out) = env.feeder_command_output(&["drop-slots"], &[]).await;
    assert_eq!(code, 0, "{out}");
    wait_for("no feeder slot", Duration::from_secs(10), || async {
        env.slots().await.is_empty()
    })
    .await;
    env.cleanup().await;
}
