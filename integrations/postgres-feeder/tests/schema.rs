//! Unsupported schemas and schema changes (design document, section 4.3).

mod common;

use common::{Env, preload, wait_for};
use std::time::Duration;

/// C9: retyping a required column stops the feeder with exit code 3 and a
/// clear message; nothing past the last applied change is confirmed; after
/// the fix, drop-slots and a restart rebuild and converge.
#[tokio::test(flavor = "multi_thread")]
async fn retyped_column_stops_then_recovers_by_rebuild() {
    let Some(mut env) = Env::new("retype").await else {
        return;
    };
    preload(&env, 100, 150).await;
    env.start_feeder(None);
    env.converge().await;
    let applied = env.stats().await["source_position"].clone();

    env.sql("ALTER TABLE knotrel_graph.edges ALTER COLUMN enabled DROP DEFAULT; \
             ALTER TABLE knotrel_graph.edges ALTER COLUMN enabled TYPE integer USING enabled::integer; \
             INSERT INTO knotrel_graph.edges VALUES (1, 99, 1);")
        .await;
    let code = env.wait_feeder(Duration::from_secs(30)).await;
    let log = env.log();
    assert_eq!(code, Some(3), "{log}");
    assert!(
        log.contains("unsupported schema change on knotrel_graph.edges")
            && log.contains("\"enabled\"")
            && log.contains("OID 23"),
        "{log}"
    );
    assert_eq!(
        env.stats().await["source_position"],
        applied,
        "nothing applied after the change"
    );
    // Revert the schema. The change was not confirmed: a restarted feeder
    // passes the startup checks, receives the change again (described with
    // the catalog of the time it was written) and stops the same way.
    env.sql(
        "ALTER TABLE knotrel_graph.edges ALTER COLUMN enabled TYPE boolean USING enabled <> 0; \
             ALTER TABLE knotrel_graph.edges ALTER COLUMN enabled SET DEFAULT true;",
    )
    .await;
    wait_for("slot inactive", Duration::from_secs(10), || async {
        env.slots().await.iter().all(|(_, active, _, _)| !active)
    })
    .await;
    env.start_feeder(None);
    assert_eq!(env.wait_feeder(Duration::from_secs(30)).await, Some(3));
    assert_eq!(
        env.log().matches("type OID 23").count(),
        2,
        "the change must be resent"
    );

    // Recovery: drop the feeder's slot and restart: gap, then rebuild.
    wait_for("slot inactive", Duration::from_secs(10), || async {
        env.slots().await.iter().all(|(_, active, _, _)| !active)
    })
    .await;
    let (code, out) = env.feeder_command_output(&["drop-slots"], &[]).await;
    assert_eq!(code, 0, "{out}");
    env.start_feeder(None);
    env.converge().await;
    assert!(env.log().contains("declared a source gap"));
    env.cleanup().await;
}

/// C9: a dropped required column stops the feeder with exit code 3.
#[tokio::test(flavor = "multi_thread")]
async fn dropped_column_stops_with_clear_error() {
    let Some(mut env) = Env::new("dropcol").await else {
        return;
    };
    preload(&env, 50, 60).await;
    env.start_feeder(None);
    env.converge().await;
    env.sql(
        "ALTER TABLE knotrel_graph.nodes DROP COLUMN active; \
             INSERT INTO knotrel_graph.nodes VALUES (5000);",
    )
    .await;
    let code = env.wait_feeder(Duration::from_secs(30)).await;
    let log = env.log();
    assert_eq!(code, Some(3), "{log}");
    assert!(
        log.contains("unsupported schema change on knotrel_graph.nodes: required column \"active\" is missing"),
        "{log}"
    );
    env.cleanup().await;
}

/// Startup validation names the problem and exits with code 3.
#[tokio::test(flavor = "multi_thread")]
async fn startup_rejects_unsupported_schema() {
    let Some(env) = Env::new("startup").await else {
        return;
    };
    let (code, out) = env.feeder_command_output(&["check"], &[]).await;
    assert_eq!(code, 0, "{out}");

    env.sql("ALTER TABLE knotrel_graph.edges DROP CONSTRAINT edges_check")
        .await;
    let (code, out) = env.feeder_command_output(&["check"], &[]).await;
    assert_eq!(code, 3, "{out}");
    assert!(out.contains("missing CHECK (source < target)"), "{out}");
    env.sql("ALTER TABLE knotrel_graph.edges ADD CHECK (source < target)")
        .await;

    env.sql("ALTER PUBLICATION knotrel_graph SET (publish = 'insert, update, delete')")
        .await;
    let (code, out) = env.feeder_command_output(&["check"], &[]).await;
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("must publish insert, update, delete and truncate"),
        "{out}"
    );
    env.sql("ALTER PUBLICATION knotrel_graph SET (publish = 'insert, update, delete, truncate')")
        .await;

    env.sql("ALTER TABLE knotrel_graph.nodes REPLICA IDENTITY NOTHING")
        .await;
    let (code, out) = env.feeder_command_output(&["check"], &[]).await;
    assert_eq!(code, 3, "{out}");
    assert!(
        out.contains("replica identity must be DEFAULT or FULL"),
        "{out}"
    );
    env.cleanup().await;
}
