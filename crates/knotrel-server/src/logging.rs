//! Structured request correlation and synchronization diagnostics (M6.3).
//!
//! Every HTTP request runs inside a `request` span carrying a process-unique
//! `request_id`, the method and the matched route. Blocking graph workers
//! enter the span of the request that submitted them, so worker log lines
//! (rejections, synchronization transitions, worker failures) carry the same
//! `request_id` as the request line. Handlers that return response metadata
//! record `generation`, `state_version` and `sync_status` on the span.
//!
//! Logs never contain graph payloads (node IDs, edges, operations, event
//! bodies), feeder session IDs or caller-supplied gap reasons. Caller-supplied
//! identifiers that operators need for correlation (`source_id`, source
//! positions) pass through [`sanitize`], which escapes control characters and
//! truncates them to [`MAX_LOGGED_CHARS`] characters. See
//! `docs/observability-contract.md`, section 17.

use crate::service::SyncStatus;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Name of the per-request span.
pub(crate) const REQUEST_SPAN: &str = "request";

/// Longest caller-supplied identifier written to a log, in characters.
pub(crate) const MAX_LOGGED_CHARS: usize = 64;

/// Source of request IDs: unique within the process, starting at 1.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Returns the next process-unique request ID.
///
/// Relaxed ordering suffices: only uniqueness is required, not ordering with
/// other memory operations. Wrapping after 2^64 requests is not a concern.
pub(crate) fn next_request_id() -> u64 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

/// A caller-supplied identifier made safe for a log line.
///
/// Displays at most [`MAX_LOGGED_CHARS`] characters, escaping control and
/// other non-printable characters as `char::escape_debug` does, and appends
/// `…` when it truncated. Formatting is lazy: nothing is allocated unless the
/// log line is actually written. O(min(n, [`MAX_LOGGED_CHARS`])) time.
#[derive(Clone, Copy)]
pub(crate) struct Sanitized<'a>(&'a str);

/// Wraps a caller-supplied identifier for logging; see [`Sanitized`].
pub(crate) fn sanitize(value: &str) -> Sanitized<'_> {
    Sanitized(value)
}

impl fmt::Display for Sanitized<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut chars = self.0.chars();
        for c in chars.by_ref().take(MAX_LOGGED_CHARS) {
            if c.is_control() || c == '\u{2028}' || c == '\u{2029}' {
                write!(f, "{}", c.escape_debug())?;
            } else {
                fmt::Write::write_char(f, c)?;
            }
        }
        if chars.next().is_some() {
            f.write_str("…")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Sanitized<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The synchronization coordinates observed before a worker's graph work.
///
/// Comparing it with the state afterwards detects status transitions and
/// generation changes; see [`SyncPoint::log_transition`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SyncPoint {
    pub(crate) generation: u64,
    pub(crate) status: SyncStatus,
}

impl SyncPoint {
    /// Logs a synchronization transition from `self` to `after`, if any.
    ///
    /// `desynchronized` and `stale` are logged at `warn`, every other
    /// transition at `info`. A generation change without a status change (a
    /// replacement import published while already `catching_up`) is logged
    /// too. `job` names the worker kind; `source_position` is the applied
    /// position after the work, sanitized.
    pub(crate) fn log_transition(
        self,
        after: SyncPoint,
        job: &'static str,
        source_position: Option<&str>,
    ) {
        if self == after {
            return;
        }
        let position = source_position.map(sanitize);
        if matches!(after.status, SyncStatus::Desynchronized | SyncStatus::Stale) {
            tracing::warn!(
                job,
                from = self.status.as_str(),
                to = after.status.as_str(),
                from_generation = self.generation,
                generation = after.generation,
                source_position = ?position,
                "synchronization state changed"
            );
        } else {
            tracing::info!(
                job,
                from = self.status.as_str(),
                to = after.status.as_str(),
                from_generation = self.generation,
                generation = after.generation,
                source_position = ?position,
                "synchronization state changed"
            );
        }
    }
}

/// Logs a rejected feeder request by its stable error code only.
///
/// The error message is not logged: several messages quote caller-supplied
/// values (instance IDs, positions, event IDs).
pub(crate) fn log_feeder_rejection(code: &'static str) {
    tracing::info!(code, "feeder request rejected");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_unique_and_increasing() {
        let first = next_request_id();
        let second = next_request_id();
        assert!(second > first);
    }

    #[test]
    fn sanitize_escapes_control_characters_and_truncates() {
        assert_eq!(sanitize("kafka:3:1200").to_string(), "kafka:3:1200");
        assert_eq!(sanitize("a\nb\u{1b}[31m").to_string(), "a\\nb\\u{1b}[31m");
        let long = "x".repeat(MAX_LOGGED_CHARS + 10);
        let shown = sanitize(&long).to_string();
        assert_eq!(shown.chars().count(), MAX_LOGGED_CHARS + 1);
        assert!(shown.ends_with('…'));
        let exact = "y".repeat(MAX_LOGGED_CHARS);
        assert_eq!(sanitize(&exact).to_string(), exact);
        // Multi-byte characters are counted as characters, never split.
        let wide = "é".repeat(MAX_LOGGED_CHARS + 1);
        assert_eq!(
            sanitize(&wide).to_string().chars().count(),
            MAX_LOGGED_CHARS + 1
        );
    }

    #[test]
    fn unchanged_sync_point_logs_nothing_and_changes_compare_unequal() {
        let before = SyncPoint {
            generation: 1,
            status: SyncStatus::CatchingUp,
        };
        let same = before;
        assert_eq!(before, same);
        let ready = SyncPoint {
            generation: 1,
            status: SyncStatus::Ready,
        };
        assert_ne!(before, ready);
        // No subscriber: must not panic either way.
        before.log_transition(same, "barrier", None);
        before.log_transition(ready, "barrier", Some("p\n1"));
    }
}
