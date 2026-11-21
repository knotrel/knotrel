//! Upgrade between minor versions (roadmap 0.9.5, docs/deployment.md,
//! "Upgrade between minor versions"): a server of the previous release is
//! fed and converged, stopped with SIGTERM, replaced by the current build on
//! the same port, and the feeder's rebuild converges again.
//!
//! Needs `KNOTREL_PREVIOUS_BIN`, a `knotrel-server` built from the previous
//! release tag; skipped without it unless `KNOTREL_UPGRADE_TEST_REQUIRED=1`.

mod common;

use common::{Env, Writers, preload};
use std::path::PathBuf;
use std::time::Duration;

fn binaries() -> Option<(PathBuf, PathBuf)> {
    let previous = match std::env::var("KNOTREL_PREVIOUS_BIN") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            if std::env::var("KNOTREL_UPGRADE_TEST_REQUIRED").is_ok_and(|v| v == "1") {
                panic!("KNOTREL_PREVIOUS_BIN is required (KNOTREL_UPGRADE_TEST_REQUIRED=1)");
            }
            eprintln!("skipped: set KNOTREL_PREVIOUS_BIN to a server of the previous release");
            return None;
        }
    };
    let current = std::env::var("KNOTREL_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/knotrel-server")
        });
    Some((previous, current))
}

#[tokio::test(flavor = "multi_thread")]
async fn upgrade_from_previous_release() {
    let Some((previous, current)) = binaries() else {
        return;
    };
    let Some(mut env) = Env::new("upgrade").await else {
        return;
    };
    // Run the previous release first.
    env.replace_server(&previous).await;
    let old_info = common::get_json(&format!("{}/v1/info", env.server.url())).await;
    preload(&env, 400, 800).await;
    env.start_feeder(None);
    env.converge().await;

    // Upgrade under writes: drain the old server, start the new one.
    let writers = Writers::start(&env, 3, 450, 41).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let drained = env.replace_server(&current).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    writers.stop().await;
    assert_eq!(
        drained,
        Some(0),
        "the previous server must drain and exit 0"
    );

    env.converge().await;
    let new_info = common::get_json(&format!("{}/v1/info", env.server.url())).await;
    assert_ne!(
        old_info["instance_id"], new_info["instance_id"],
        "a new process"
    );
    assert_eq!(
        env.stats().await["generation"],
        1,
        "rebuilt by one create_only import"
    );
    assert!(
        env.log().contains("rebuilding on the new instance"),
        "the feeder rebuilt"
    );
    env.cleanup().await;
}
