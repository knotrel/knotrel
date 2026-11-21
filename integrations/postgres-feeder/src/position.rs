//! Source identities and positions sent to Knotrel.
//!
//! Knotrel treats positions as opaque strings. The feeder defines their format
//! and is the only party that parses them (design document, section 6):
//!
//! ```text
//! source_epoch    = pg1;sys=<system identifier>;tli=<timeline>;slot=<slot>
//! source_position = <source_epoch>;lsn=<commit LSN>;i=<n>[;end]
//! ```
//!
//! Within one lineage (one `source_epoch`), positions are totally ordered by
//! `(commit LSN, n)`: logical decoding emits transactions in commit order and
//! commit LSNs increase, and `n` numbers the events of one transaction.

use pgwire_replication::Lsn;
use std::cmp::Ordering;
use std::fmt;

/// Maximum length of a slot name accepted by PostgreSQL (`NAMEDATALEN - 1`).
pub const MAX_SLOT_NAME_LEN: usize = 63;

/// The source lineage: one replication slot on one timeline of one cluster.
///
/// A new bootstrap always creates a new slot, hence a new lineage. Events of
/// an older lineage are rejected by Knotrel (`source_epoch_mismatch`) once a
/// newer lineage was imported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lineage {
    /// PostgreSQL system identifier (`IDENTIFY_SYSTEM`).
    pub system_id: String,
    /// Timeline at bootstrap (`IDENTIFY_SYSTEM`).
    pub timeline: u32,
    /// Replication slot that produced the snapshot and the stream.
    pub slot: String,
}

impl Lineage {
    /// The `source_epoch` token sent to Knotrel.
    pub fn source_epoch(&self) -> String {
        format!(
            "pg1;sys={};tli={};slot={}",
            self.system_id, self.timeline, self.slot
        )
    }
}

/// A position in the change stream of one lineage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    /// The lineage this position belongs to.
    pub lineage: Lineage,
    /// Commit LSN of the transaction, or the slot's consistent point for the
    /// snapshot position.
    pub lsn: Lsn,
    /// Index of the event in its transaction (1-based); 0 for the snapshot.
    pub index: u32,
    /// Whether this is the last event of its transaction (always true for the
    /// snapshot position).
    pub end: bool,
}

impl Position {
    /// The position of a snapshot taken at the slot's consistent point.
    pub fn snapshot(lineage: Lineage, consistent_point: Lsn) -> Self {
        Self {
            lineage,
            lsn: consistent_point,
            index: 0,
            end: true,
        }
    }

    /// Order of two positions of the same lineage, by `(lsn, index)`.
    pub fn stream_cmp(&self, lsn: Lsn, index: u32) -> Ordering {
        (self.lsn.as_u64(), self.index).cmp(&(lsn.as_u64(), index))
    }

    /// Parses a position produced by [`Position`]'s `Display`.
    ///
    /// # Errors
    ///
    /// Returns a description of the first malformed or missing field. A
    /// position written by another program (or another format version) is
    /// rejected rather than guessed.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut fields = text.split(';');
        if fields.next() != Some("pg1") {
            return Err(format!("not a pg1 position: {text:?}"));
        }
        let mut take = |key: &str| -> Result<&str, String> {
            let field = fields
                .next()
                .ok_or_else(|| format!("position {text:?} has no {key} field"))?;
            field
                .strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
                .ok_or_else(|| format!("position {text:?}: expected {key}=..., found {field:?}"))
        };
        let system_id = take("sys")?.to_string();
        let timeline = take("tli")?
            .parse::<u32>()
            .map_err(|e| format!("position {text:?}: bad timeline: {e}"))?;
        let slot = take("slot")?.to_string();
        let lsn = Lsn::parse(take("lsn")?).map_err(|e| format!("position {text:?}: {e}"))?;
        let index = take("i")?
            .parse::<u32>()
            .map_err(|e| format!("position {text:?}: bad index: {e}"))?;
        let end = match fields.next() {
            None => false,
            Some("end") => true,
            Some(other) => return Err(format!("position {text:?}: unexpected field {other:?}")),
        };
        if let Some(extra) = fields.next() {
            return Err(format!("position {text:?}: unexpected field {extra:?}"));
        }
        if system_id.is_empty() || !is_valid_slot_name(&slot) {
            return Err(format!("position {text:?}: bad system identifier or slot"));
        }
        Ok(Self {
            lineage: Lineage {
                system_id,
                timeline,
                slot,
            },
            lsn,
            index,
            end,
        })
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{};lsn={};i={}",
            self.lineage.source_epoch(),
            self.lsn,
            self.index
        )?;
        if self.end {
            f.write_str(";end")?;
        }
        Ok(())
    }
}

/// Event identifier: `<commit LSN>.<n>`, unique within one lineage.
pub fn event_id(lsn: Lsn, index: u32) -> String {
    format!("{lsn}.{index}")
}

/// Whether `name` is a valid PostgreSQL replication slot name: 1 to 63
/// characters from `[a-z0-9_]`.
pub fn is_valid_slot_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SLOT_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lineage() -> Lineage {
        Lineage {
            system_id: "7301234567890123456".into(),
            timeline: 1,
            slot: "knotrel_feeder_ab12".into(),
        }
    }

    #[test]
    fn positions_round_trip() {
        for (index, end) in [(0, true), (3, false), (7, true)] {
            let p = Position {
                lineage: lineage(),
                lsn: Lsn::parse("0/16B3740").unwrap(),
                index,
                end,
            };
            assert_eq!(Position::parse(&p.to_string()).unwrap(), p);
        }
    }

    #[test]
    fn format_is_stable_and_short() {
        let p = Position::snapshot(lineage(), Lsn::parse("1/A").unwrap());
        assert_eq!(
            p.to_string(),
            "pg1;sys=7301234567890123456;tli=1;slot=knotrel_feeder_ab12;lsn=1/A;i=0;end"
        );
        assert!(p.to_string().len() < 256, "Knotrel limit is 256 bytes");
    }

    #[test]
    fn foreign_or_malformed_positions_are_rejected() {
        for bad in [
            "pos-01",
            "pg2;sys=1;tli=1;slot=s;lsn=0/1;i=1",
            "pg1;sys=1;tli=x;slot=s;lsn=0/1;i=1",
            "pg1;sys=1;tli=1;slot=S;lsn=0/1;i=1",
            "pg1;sys=1;tli=1;slot=s;lsn=zz;i=1",
            "pg1;sys=1;tli=1;slot=s;lsn=0/1;i=1;fin",
            "pg1;sys=1;tli=1;slot=s;lsn=0/1;i=1;end;more",
            "pg1;tli=1;slot=s;lsn=0/1;i=1",
        ] {
            assert!(Position::parse(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn stream_order_is_lsn_then_index() {
        let p = Position {
            lineage: lineage(),
            lsn: Lsn::from_u64(100),
            index: 2,
            end: false,
        };
        assert_eq!(p.stream_cmp(Lsn::from_u64(100), 2), Ordering::Equal);
        assert_eq!(p.stream_cmp(Lsn::from_u64(100), 3), Ordering::Less);
        assert_eq!(p.stream_cmp(Lsn::from_u64(99), 9), Ordering::Greater);
        assert_eq!(p.stream_cmp(Lsn::from_u64(101), 1), Ordering::Less);
    }

    #[test]
    fn slot_names_follow_postgres_rules() {
        assert!(is_valid_slot_name("knotrel_feeder_0a"));
        assert!(!is_valid_slot_name(""));
        assert!(!is_valid_slot_name("Upper"));
        assert!(!is_valid_slot_name("with-dash"));
        assert!(!is_valid_slot_name(&"a".repeat(64)));
    }
}
