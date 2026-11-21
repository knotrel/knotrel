//! Fault injection for the recovery tests.
//!
//! `KNOTREL_PG_FEEDER_FAULT=<point>[:<n>]` arms one named point. The `n`-th
//! time (default: first) the feeder reaches it:
//!
//! - a crash point makes the process abort, as a crash would: no cleanup, no
//!   acknowledgement;
//! - `lose_events_response` discards a successful Knotrel answer, as if the
//!   connection had dropped after Knotrel applied the events;
//! - `slow_bootstrap` pauses the bootstrap after the image is read for three
//!   lease durations, as a large image would.
//!
//! Unset, every point is a no-op. The points are listed in [`POINTS`].

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// The named fault points.
pub const POINTS: &[&str] = &[
    "after_slot_created",
    "after_snapshot_read",
    "after_import",
    "before_events",
    "after_events_applied",
    "after_ack",
    "lose_events_response",
    "slow_bootstrap",
];

struct Armed {
    point: String,
    hit_number: u64,
    hits: AtomicU64,
}

fn armed() -> Option<&'static Armed> {
    static ARMED: OnceLock<Option<Armed>> = OnceLock::new();
    ARMED
        .get_or_init(|| {
            let spec = std::env::var("KNOTREL_PG_FEEDER_FAULT").ok()?;
            let (point, n) = match spec.split_once(':') {
                Some((p, n)) => (p.to_string(), n.parse().unwrap_or(1)),
                None => (spec, 1),
            };
            if !POINTS.contains(&point.as_str()) {
                eprintln!("KNOTREL_PG_FEEDER_FAULT: unknown point {point:?}; known: {POINTS:?}");
                std::process::exit(2);
            }
            Some(Armed {
                point,
                hit_number: n.max(1),
                hits: AtomicU64::new(0),
            })
        })
        .as_ref()
}

fn triggered(point: &str) -> bool {
    armed().is_some_and(|a| {
        a.point == point && a.hits.fetch_add(1, Ordering::SeqCst) + 1 == a.hit_number
    })
}

/// Aborts the process if the crash point `point` is armed and reached for the
/// configured time.
pub fn hit(point: &str) {
    if triggered(point) {
        eprintln!("fault injection: aborting at {point}");
        std::process::abort();
    }
}

/// Whether the answer at `point` must be treated as lost this time.
pub fn lose(point: &str) -> bool {
    let lost = triggered(point);
    if lost {
        eprintln!("fault injection: discarding the answer at {point}");
    }
    lost
}

/// Sleeps for `duration` if the pause point `point` is armed and reached for
/// the configured time.
pub async fn pause(point: &str, duration: std::time::Duration) {
    if triggered(point) {
        eprintln!(
            "fault injection: pausing {} ms at {point}",
            duration.as_millis()
        );
        tokio::time::sleep(duration).await;
    }
}
