//! Translation of row changes into Knotrel graph operations.
//!
//! The translation is a pure function of the change record and the relation
//! description (design document, section 4.2): it reads no feeder state, so
//! a transaction resent by PostgreSQL after a reconnection produces exactly
//! the same events, which the feeder can then skip by position.

use crate::pgoutput::{Datum, Message, Relation, Tuple};
use std::collections::HashMap;
use std::fmt;

/// PostgreSQL type OID of `bigint`.
pub const INT8_OID: u32 = 20;
/// PostgreSQL type OID of `boolean`.
pub const BOOL_OID: u32 = 16;

/// One Knotrel feeder operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphOp {
    /// Register a node (active).
    AddNode(u64),
    /// Remove a node and its incident edges.
    RemoveNode(u64),
    /// Set a node's `active` flag.
    SetNodeActive(u64, bool),
    /// Register the undirected edge between two nodes (enabled).
    Link(u64, u64),
    /// Remove the undirected edge.
    Cut(u64, u64),
    /// Set an edge's `enabled` flag.
    SetEdgeEnabled(u64, u64, bool),
}

impl GraphOp {
    /// The operation fields of a Knotrel feeder event.
    pub fn json_fields(&self, event: &mut serde_json::Map<String, serde_json::Value>) {
        use serde_json::Value::{Bool, String as S};
        let id = |n: &u64| S(n.to_string());
        let (op, fields): (&str, Vec<(&str, serde_json::Value)>) = match self {
            Self::AddNode(n) => ("add_node", vec![("node", id(n))]),
            Self::RemoveNode(n) => ("remove_node", vec![("node", id(n))]),
            Self::SetNodeActive(n, a) => (
                "set_node_active",
                vec![("node", id(n)), ("active", Bool(*a))],
            ),
            Self::Link(s, t) => ("link", vec![("source", id(s)), ("target", id(t))]),
            Self::Cut(s, t) => ("cut", vec![("source", id(s)), ("target", id(t))]),
            Self::SetEdgeEnabled(s, t, e) => (
                "set_edge_enabled",
                vec![("source", id(s)), ("target", id(t)), ("enabled", Bool(*e))],
            ),
        };
        event.insert("op".into(), S(op.into()));
        for (k, v) in fields {
            event.insert(k.into(), v);
        }
    }
}

/// The configured names of the two graph tables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableNames {
    /// Schema of both tables.
    pub schema: String,
    /// Node table.
    pub nodes: String,
    /// Edge table.
    pub edges: String,
}

/// Why a change cannot be translated.
#[derive(Debug, PartialEq, Eq)]
pub enum MappingError {
    /// The relation no longer has the required structure.
    Schema(String),
    /// A value outside the mapping (negative ID, `NULL` flag, self-loop).
    Value(String),
}

impl fmt::Display for MappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Schema(m) => write!(f, "unsupported schema change on {m}"),
            Self::Value(m) => write!(f, "unsupported value: {m}"),
        }
    }
}

impl std::error::Error for MappingError {}

/// What a change message means for the feeder.
#[derive(Debug, PartialEq, Eq)]
pub enum Translation {
    /// Graph operations, in order (possibly none).
    Ops(Vec<GraphOp>),
    /// A graph table was truncated: the feeder must rebuild.
    Truncate,
}

#[derive(Clone, Copy, Debug)]
enum Layout {
    Nodes {
        id: usize,
        active: usize,
    },
    Edges {
        source: usize,
        target: usize,
        enabled: usize,
    },
}

/// Relation descriptions received on the stream, validated against the
/// expected schema.
#[derive(Debug)]
pub struct Relations {
    names: TableNames,
    by_oid: HashMap<u32, (String, Layout)>,
}

impl Relations {
    /// An empty registry for the given table names.
    pub fn new(names: TableNames) -> Self {
        Self {
            names,
            by_oid: HashMap::new(),
        }
    }

    /// Validates and records a relation description.
    ///
    /// # Errors
    ///
    /// [`MappingError::Schema`] if a graph table lost a required column,
    /// changed a column type or key, changed its replica identity to an
    /// unsupported setting, or was renamed.
    pub fn register(&mut self, rel: &Relation) -> Result<(), MappingError> {
        let qualified = format!("{}.{}", rel.namespace, rel.name);
        let expected_nodes = rel.namespace == self.names.schema && rel.name == self.names.nodes;
        let expected_edges = rel.namespace == self.names.schema && rel.name == self.names.edges;
        if !expected_nodes && !expected_edges {
            if let Some((previous, _)) = self.by_oid.get(&rel.id) {
                return Err(MappingError::Schema(format!(
                    "{previous}: table renamed to {qualified}"
                )));
            }
            return Err(MappingError::Schema(format!(
                "{qualified}: table is in the publication but is not a graph table"
            )));
        }
        let layout = validate(rel, &qualified, expected_nodes)?;
        self.by_oid.insert(rel.id, (qualified, layout));
        Ok(())
    }

    /// Translates one decoded message.
    ///
    /// Returns `Ok(None)` for messages that are not row changes.
    ///
    /// # Errors
    ///
    /// [`MappingError::Value`] for a value outside the mapping, and
    /// [`MappingError::Schema`] for a change on a relation never described.
    pub fn translate(&self, message: &Message) -> Result<Option<Translation>, MappingError> {
        let (relation, change) = match message {
            Message::Insert { relation, new } => (*relation, Change::Insert(new)),
            Message::Update { relation, old, new } => {
                (*relation, Change::Update(old.as_ref(), new))
            }
            Message::Delete { relation, old } => (*relation, Change::Delete(old)),
            Message::Truncate { relations } => {
                return if relations.iter().any(|r| self.by_oid.contains_key(r)) {
                    Ok(Some(Translation::Truncate))
                } else {
                    Err(MappingError::Schema(format!(
                        "truncate of relations {relations:?} that were never described"
                    )))
                };
            }
            Message::Relation(_) | Message::Ignored(_) => return Ok(None),
        };
        let (name, layout) = self.by_oid.get(&relation).ok_or_else(|| {
            MappingError::Schema(format!(
                "relation {relation}: change before its description"
            ))
        })?;
        Ok(Some(Translation::Ops(translate_change(
            name, *layout, change,
        )?)))
    }
}

enum Change<'a> {
    Insert(&'a Tuple),
    Update(Option<&'a Tuple>, &'a Tuple),
    Delete(&'a Tuple),
}

fn validate(rel: &Relation, qualified: &str, nodes: bool) -> Result<Layout, MappingError> {
    let full = match rel.replica_identity {
        b'd' => false,
        b'f' => true,
        other => {
            return Err(MappingError::Schema(format!(
                "{qualified}: replica identity is {}, expected DEFAULT or FULL",
                match other {
                    b'n' => "NOTHING".to_string(),
                    b'i' => "USING INDEX".to_string(),
                    o => format!("{:?}", o as char),
                }
            )));
        }
    };
    let column = |name: &str, type_oid: u32, key: bool| -> Result<usize, MappingError> {
        let (index, col) = rel
            .columns
            .iter()
            .enumerate()
            .find(|(_, c)| c.name == name)
            .ok_or_else(|| {
                MappingError::Schema(format!("{qualified}: required column {name:?} is missing"))
            })?;
        if col.type_oid != type_oid {
            return Err(MappingError::Schema(format!(
                "{qualified}: column {name:?} has type OID {}, expected {}",
                col.type_oid,
                if type_oid == INT8_OID {
                    "bigint (OID 20)"
                } else {
                    "boolean (OID 16)"
                }
            )));
        }
        if !full && col.key != key {
            return Err(MappingError::Schema(format!(
                "{qualified}: column {name:?} {} the primary key",
                if key {
                    "is no longer part of"
                } else {
                    "became part of"
                }
            )));
        }
        Ok(index)
    };
    Ok(if nodes {
        Layout::Nodes {
            id: column("id", INT8_OID, true)?,
            active: column("active", BOOL_OID, false)?,
        }
    } else {
        Layout::Edges {
            source: column("source", INT8_OID, true)?,
            target: column("target", INT8_OID, true)?,
            enabled: column("enabled", BOOL_OID, false)?,
        }
    })
}

fn value<'a>(
    tuple: &'a Tuple,
    index: usize,
    table: &str,
    column: &str,
) -> Result<&'a [u8], MappingError> {
    match tuple.get(index) {
        Some(Datum::Text(bytes)) => Ok(bytes),
        Some(Datum::Null) => Err(MappingError::Value(format!("{table}.{column} is NULL"))),
        Some(Datum::Unchanged) => Err(MappingError::Value(format!(
            "{table}.{column} was sent as an unchanged TOAST value"
        ))),
        None => Err(MappingError::Value(format!(
            "{table}: row has no column {column} (index {index})"
        ))),
    }
}

fn node_id(tuple: &Tuple, index: usize, table: &str, column: &str) -> Result<u64, MappingError> {
    let raw = value(tuple, index, table, column)?;
    let text = std::str::from_utf8(raw)
        .map_err(|_| MappingError::Value(format!("{table}.{column} is not UTF-8")))?;
    let id: i64 = text
        .parse()
        .map_err(|_| MappingError::Value(format!("{table}.{column} = {text:?} is not a bigint")))?;
    u64::try_from(id).map_err(|_| {
        MappingError::Value(format!(
            "{table}.{column} = {id} is negative; node IDs must be >= 0"
        ))
    })
}

fn flag(tuple: &Tuple, index: usize, table: &str, column: &str) -> Result<bool, MappingError> {
    match value(tuple, index, table, column)? {
        b"t" => Ok(true),
        b"f" => Ok(false),
        other => Err(MappingError::Value(format!(
            "{table}.{column} = {:?} is not a boolean",
            String::from_utf8_lossy(other)
        ))),
    }
}

fn edge_key(
    tuple: &Tuple,
    source: usize,
    target: usize,
    table: &str,
) -> Result<(u64, u64), MappingError> {
    let s = node_id(tuple, source, table, "source")?;
    let t = node_id(tuple, target, table, "target")?;
    if s == t {
        return Err(MappingError::Value(format!(
            "{table}: self-loop on node {s} (Knotrel graphs have none)"
        )));
    }
    Ok((s, t))
}

fn translate_change(
    table: &str,
    layout: Layout,
    change: Change<'_>,
) -> Result<Vec<GraphOp>, MappingError> {
    let mut ops = Vec::with_capacity(3);
    match layout {
        Layout::Nodes { id, active } => match change {
            Change::Insert(new) => {
                let n = node_id(new, id, table, "id")?;
                ops.push(GraphOp::AddNode(n));
                if !flag(new, active, table, "active")? {
                    ops.push(GraphOp::SetNodeActive(n, false));
                }
            }
            Change::Update(old, new) => {
                let n = node_id(new, id, table, "id")?;
                let is_active = flag(new, active, table, "active")?;
                let previous = old.map(|o| node_id(o, id, table, "id")).transpose()?;
                match previous {
                    Some(p) if p != n => {
                        ops.push(GraphOp::RemoveNode(p));
                        ops.push(GraphOp::AddNode(n));
                        if !is_active {
                            ops.push(GraphOp::SetNodeActive(n, false));
                        }
                    }
                    _ => ops.push(GraphOp::SetNodeActive(n, is_active)),
                }
            }
            Change::Delete(old) => ops.push(GraphOp::RemoveNode(node_id(old, id, table, "id")?)),
        },
        Layout::Edges {
            source,
            target,
            enabled,
        } => match change {
            Change::Insert(new) => {
                let (s, t) = edge_key(new, source, target, table)?;
                ops.push(GraphOp::Link(s, t));
                if !flag(new, enabled, table, "enabled")? {
                    ops.push(GraphOp::SetEdgeEnabled(s, t, false));
                }
            }
            Change::Update(old, new) => {
                let (s, t) = edge_key(new, source, target, table)?;
                let is_enabled = flag(new, enabled, table, "enabled")?;
                let previous = old
                    .map(|o| edge_key(o, source, target, table))
                    .transpose()?;
                match previous {
                    Some(p) if p != (s, t) => {
                        ops.push(GraphOp::Cut(p.0, p.1));
                        ops.push(GraphOp::Link(s, t));
                        if !is_enabled {
                            ops.push(GraphOp::SetEdgeEnabled(s, t, false));
                        }
                    }
                    _ => ops.push(GraphOp::SetEdgeEnabled(s, t, is_enabled)),
                }
            }
            Change::Delete(old) => {
                let (s, t) = edge_key(old, source, target, table)?;
                ops.push(GraphOp::Cut(s, t));
            }
        },
    }
    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pgoutput::Column;

    fn names() -> TableNames {
        TableNames {
            schema: "knotrel_graph".into(),
            nodes: "nodes".into(),
            edges: "edges".into(),
        }
    }

    fn col(name: &str, type_oid: u32, key: bool) -> Column {
        Column {
            key,
            name: name.into(),
            type_oid,
        }
    }

    fn nodes_rel(id: u32) -> Relation {
        Relation {
            id,
            namespace: "knotrel_graph".into(),
            name: "nodes".into(),
            replica_identity: b'd',
            columns: vec![
                col("id", INT8_OID, true),
                col("label", 25, false),
                col("active", BOOL_OID, false),
            ],
        }
    }

    fn edges_rel(id: u32) -> Relation {
        Relation {
            id,
            namespace: "knotrel_graph".into(),
            name: "edges".into(),
            replica_identity: b'd',
            columns: vec![
                col("source", INT8_OID, true),
                col("target", INT8_OID, true),
                col("enabled", BOOL_OID, false),
            ],
        }
    }

    fn t(values: &[Option<&str>]) -> Tuple {
        values
            .iter()
            .map(|v| match v {
                None => Datum::Null,
                Some(s) => Datum::Text(s.as_bytes().to_vec()),
            })
            .collect()
    }

    fn registry() -> Relations {
        let mut r = Relations::new(names());
        r.register(&nodes_rel(1)).unwrap();
        r.register(&edges_rel(2)).unwrap();
        r
    }

    fn ops(r: &Relations, m: Message) -> Vec<GraphOp> {
        match r.translate(&m).unwrap() {
            Some(Translation::Ops(ops)) => ops,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn node_changes() {
        let r = registry();
        use GraphOp::*;
        assert_eq!(
            ops(
                &r,
                Message::Insert {
                    relation: 1,
                    new: t(&[Some("5"), None, Some("t")])
                }
            ),
            vec![AddNode(5)]
        );
        assert_eq!(
            ops(
                &r,
                Message::Insert {
                    relation: 1,
                    new: t(&[Some("6"), Some("x"), Some("f")])
                }
            ),
            vec![AddNode(6), SetNodeActive(6, false)]
        );
        assert_eq!(
            ops(
                &r,
                Message::Update {
                    relation: 1,
                    old: None,
                    new: t(&[Some("6"), None, Some("t")])
                }
            ),
            vec![SetNodeActive(6, true)]
        );
        assert_eq!(
            ops(
                &r,
                Message::Update {
                    relation: 1,
                    old: Some(t(&[Some("6"), None, None])),
                    new: t(&[Some("9"), None, Some("f")])
                }
            ),
            vec![RemoveNode(6), AddNode(9), SetNodeActive(9, false)]
        );
        assert_eq!(
            ops(
                &r,
                Message::Delete {
                    relation: 1,
                    old: t(&[Some("9"), None, None])
                }
            ),
            vec![RemoveNode(9)]
        );
    }

    #[test]
    fn edge_changes() {
        let r = registry();
        use GraphOp::*;
        assert_eq!(
            ops(
                &r,
                Message::Insert {
                    relation: 2,
                    new: t(&[Some("1"), Some("2"), Some("t")])
                }
            ),
            vec![Link(1, 2)]
        );
        assert_eq!(
            ops(
                &r,
                Message::Insert {
                    relation: 2,
                    new: t(&[Some("1"), Some("3"), Some("f")])
                }
            ),
            vec![Link(1, 3), SetEdgeEnabled(1, 3, false)]
        );
        assert_eq!(
            ops(
                &r,
                Message::Update {
                    relation: 2,
                    old: None,
                    new: t(&[Some("1"), Some("3"), Some("t")])
                }
            ),
            vec![SetEdgeEnabled(1, 3, true)]
        );
        assert_eq!(
            ops(
                &r,
                Message::Update {
                    relation: 2,
                    old: Some(t(&[Some("1"), Some("3"), None])),
                    new: t(&[Some("2"), Some("3"), Some("f")])
                }
            ),
            vec![Cut(1, 3), Link(2, 3), SetEdgeEnabled(2, 3, false)]
        );
        assert_eq!(
            ops(
                &r,
                Message::Delete {
                    relation: 2,
                    old: t(&[Some("2"), Some("3"), None])
                }
            ),
            vec![Cut(2, 3)]
        );
    }

    #[test]
    fn full_replica_identity_compares_old_rows() {
        let mut r = Relations::new(names());
        let mut rel = nodes_rel(1);
        rel.replica_identity = b'f';
        for c in &mut rel.columns {
            c.key = true;
        }
        r.register(&rel).unwrap();
        assert_eq!(
            ops(
                &r,
                Message::Update {
                    relation: 1,
                    old: Some(t(&[Some("4"), None, Some("t")])),
                    new: t(&[Some("4"), None, Some("f")])
                }
            ),
            vec![GraphOp::SetNodeActive(4, false)]
        );
    }

    #[test]
    fn truncate_requires_rebuild() {
        let r = registry();
        assert_eq!(
            r.translate(&Message::Truncate { relations: vec![2] })
                .unwrap(),
            Some(Translation::Truncate)
        );
        assert!(
            r.translate(&Message::Truncate {
                relations: vec![99]
            })
            .is_err()
        );
        assert_eq!(r.translate(&Message::Ignored(b'Y')).unwrap(), None);
    }

    #[test]
    fn unsupported_schema_changes_are_named() {
        let mut r = registry();
        let mut retyped = nodes_rel(1);
        retyped.columns[2].type_oid = 23;
        let e = r.register(&retyped).unwrap_err().to_string();
        assert!(
            e.contains("knotrel_graph.nodes") && e.contains("\"active\"") && e.contains("OID 23"),
            "{e}"
        );

        let mut dropped = edges_rel(2);
        dropped.columns.pop();
        let e = r.register(&dropped).unwrap_err().to_string();
        assert!(e.contains("required column \"enabled\" is missing"), "{e}");

        let mut nothing = edges_rel(2);
        nothing.replica_identity = b'n';
        assert!(
            r.register(&nothing)
                .unwrap_err()
                .to_string()
                .contains("NOTHING")
        );

        let mut rekeyed = edges_rel(2);
        rekeyed.columns[1].key = false;
        assert!(
            r.register(&rekeyed)
                .unwrap_err()
                .to_string()
                .contains("primary key")
        );

        let mut renamed = nodes_rel(1);
        renamed.name = "vertices".into();
        assert!(
            r.register(&renamed)
                .unwrap_err()
                .to_string()
                .contains("renamed")
        );

        let mut foreign = nodes_rel(77);
        foreign.name = "other".into();
        assert!(
            r.register(&foreign)
                .unwrap_err()
                .to_string()
                .contains("not a graph table")
        );

        assert!(matches!(
            r.translate(&Message::Insert {
                relation: 55,
                new: vec![]
            }),
            Err(MappingError::Schema(_))
        ));
    }

    #[test]
    fn values_outside_the_mapping_are_rejected() {
        let r = registry();
        for (relation, row) in [
            (1, t(&[Some("-1"), None, Some("t")])),
            (1, t(&[Some("1"), None, None])),
            (1, t(&[Some("x"), None, Some("t")])),
            (1, t(&[Some("1"), None, Some("yes")])),
            (2, t(&[Some("3"), Some("3"), Some("t")])),
        ] {
            assert!(
                matches!(
                    r.translate(&Message::Insert { relation, new: row }),
                    Err(MappingError::Value(_))
                ),
                "relation {relation}"
            );
        }
    }

    #[test]
    fn json_fields_match_the_knotrel_event_schema() {
        let mut m = serde_json::Map::new();
        GraphOp::SetEdgeEnabled(18446744073709551615, 2, false).json_fields(&mut m);
        assert_eq!(
            serde_json::Value::Object(m),
            serde_json::json!({"op":"set_edge_enabled","source":"18446744073709551615","target":"2","enabled":false})
        );
        let mut m = serde_json::Map::new();
        GraphOp::RemoveNode(7).json_fields(&mut m);
        assert_eq!(
            serde_json::Value::Object(m),
            serde_json::json!({"op":"remove_node","node":"7"})
        );
    }
}
