//! Decoding of `pgoutput` protocol version 1 change messages.
//!
//! The replication client delivers transaction boundaries (`Begin`, `Commit`)
//! itself; this module decodes the remaining messages the feeder needs:
//! `Relation`, `Insert`, `Update`, `Delete` and `Truncate`. `Type` and
//! `Origin` messages are recognized and ignored. Only the text tuple format is
//! used (the feeder never asks for binary output).
//!
//! Reference: PostgreSQL documentation, "Logical Replication Message
//! Formats".

/// A decoded change-stream message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// Description of a relation; sent before its first change and again
    /// after any change of its definition.
    Relation(Relation),
    /// A new row.
    Insert {
        /// Relation OID.
        relation: u32,
        /// The new row.
        new: Tuple,
    },
    /// A changed row.
    Update {
        /// Relation OID.
        relation: u32,
        /// The old row: key columns only (`K`) when a key column changed
        /// under `REPLICA IDENTITY DEFAULT`, the whole row (`O`) under
        /// `REPLICA IDENTITY FULL`, absent otherwise.
        old: Option<Tuple>,
        /// The new row.
        new: Tuple,
    },
    /// A deleted row, as its key columns (`K`) or whole row (`O`).
    Delete {
        /// Relation OID.
        relation: u32,
        /// The old row.
        old: Tuple,
    },
    /// `TRUNCATE` of one or more relations.
    Truncate {
        /// OIDs of the truncated relations.
        relations: Vec<u32>,
    },
    /// A message the feeder does not need (`Type`, `Origin`, `Message`).
    Ignored(u8),
}

/// A relation description (`R` message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relation {
    /// Relation OID.
    pub id: u32,
    /// Schema name.
    pub namespace: String,
    /// Table name.
    pub name: String,
    /// Replica identity setting: `d` default, `n` nothing, `f` full, `i` index.
    pub replica_identity: u8,
    /// Columns in table order.
    pub columns: Vec<Column>,
}

/// One column of a relation description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    /// Whether the column is part of the replica identity key.
    pub key: bool,
    /// Column name.
    pub name: String,
    /// Type OID.
    pub type_oid: u32,
}

/// The column values of one row, in relation column order.
pub type Tuple = Vec<Datum>;

/// One column value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Datum {
    /// SQL `NULL`.
    Null,
    /// An unchanged TOASTed value whose content was not sent.
    Unchanged,
    /// A value in text format.
    Text(Vec<u8>),
}

/// A malformed message.
#[derive(Debug, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed pgoutput message: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8], DecodeError> {
        if self.data.len() < n {
            return Err(DecodeError(format!("truncated {what}")));
        }
        let (head, tail) = self.data.split_at(n);
        self.data = tail;
        Ok(head)
    }

    fn u8(&mut self, what: &str) -> Result<u8, DecodeError> {
        Ok(self.take(1, what)?[0])
    }

    fn i16(&mut self, what: &str) -> Result<i16, DecodeError> {
        let b = self.take(2, what)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self, what: &str) -> Result<u32, DecodeError> {
        let b = self.take(4, what)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self, what: &str) -> Result<i32, DecodeError> {
        Ok(self.u32(what)? as i32)
    }

    fn cstring(&mut self, what: &str) -> Result<String, DecodeError> {
        let end = self
            .data
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| DecodeError(format!("unterminated {what}")))?;
        let text = std::str::from_utf8(&self.data[..end])
            .map_err(|_| DecodeError(format!("{what} is not UTF-8")))?
            .to_string();
        self.data = &self.data[end + 1..];
        Ok(text)
    }

    fn tuple(&mut self) -> Result<Tuple, DecodeError> {
        let count = self.i16("tuple column count")?;
        let count =
            usize::try_from(count).map_err(|_| DecodeError("negative column count".into()))?;
        let mut tuple = Vec::with_capacity(count);
        for _ in 0..count {
            tuple.push(match self.u8("column kind")? {
                b'n' => Datum::Null,
                b'u' => Datum::Unchanged,
                b't' => {
                    let len = self.i32("value length")?;
                    let len = usize::try_from(len)
                        .map_err(|_| DecodeError("negative value length".into()))?;
                    Datum::Text(self.take(len, "value")?.to_vec())
                }
                b'b' => return Err(DecodeError("binary tuple data was not requested".into())),
                other => return Err(DecodeError(format!("unknown column kind {other:#x}"))),
            });
        }
        Ok(tuple)
    }

    fn finish(&self, what: &str) -> Result<(), DecodeError> {
        if self.data.is_empty() {
            Ok(())
        } else {
            Err(DecodeError(format!(
                "{} trailing bytes after {what}",
                self.data.len()
            )))
        }
    }
}

/// Decodes one `pgoutput` message (the payload of an `XLogData` frame).
///
/// # Errors
///
/// [`DecodeError`] for a truncated, malformed or binary-format message.
pub fn decode(data: &[u8]) -> Result<Message, DecodeError> {
    let mut r = Reader { data };
    let tag = r.u8("message tag")?;
    let message = match tag {
        b'R' => {
            let id = r.u32("relation id")?;
            let namespace = r.cstring("namespace")?;
            let name = r.cstring("relation name")?;
            let replica_identity = r.u8("replica identity")?;
            let count = r.i16("column count")?;
            let count =
                usize::try_from(count).map_err(|_| DecodeError("negative column count".into()))?;
            let mut columns = Vec::with_capacity(count);
            for _ in 0..count {
                let flags = r.u8("column flags")?;
                let name = r.cstring("column name")?;
                let type_oid = r.u32("column type")?;
                let _typmod = r.i32("column type modifier")?;
                columns.push(Column {
                    key: flags & 1 == 1,
                    name,
                    type_oid,
                });
            }
            Message::Relation(Relation {
                id,
                namespace,
                name,
                replica_identity,
                columns,
            })
        }
        b'I' => {
            let relation = r.u32("relation id")?;
            if r.u8("new tuple marker")? != b'N' {
                return Err(DecodeError("insert without new tuple".into()));
            }
            Message::Insert {
                relation,
                new: r.tuple()?,
            }
        }
        b'U' => {
            let relation = r.u32("relation id")?;
            let mut marker = r.u8("tuple marker")?;
            let mut old = None;
            if marker == b'K' || marker == b'O' {
                old = Some(r.tuple()?);
                marker = r.u8("new tuple marker")?;
            }
            if marker != b'N' {
                return Err(DecodeError("update without new tuple".into()));
            }
            Message::Update {
                relation,
                old,
                new: r.tuple()?,
            }
        }
        b'D' => {
            let relation = r.u32("relation id")?;
            let marker = r.u8("old tuple marker")?;
            if marker != b'K' && marker != b'O' {
                return Err(DecodeError("delete without old tuple".into()));
            }
            Message::Delete {
                relation,
                old: r.tuple()?,
            }
        }
        b'T' => {
            let count = r.u32("relation count")?;
            let _options = r.u8("truncate options")?;
            let mut relations = Vec::new();
            for _ in 0..count {
                relations.push(r.u32("truncated relation id")?);
            }
            Message::Truncate { relations }
        }
        b'Y' | b'O' | b'M' => return Ok(Message::Ignored(tag)),
        other => return Err(DecodeError(format!("unexpected message tag {other:#x}"))),
    };
    r.finish("message")?;
    Ok(message)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encodes a relation message, for tests.
    pub(crate) fn relation_bytes(rel: &Relation) -> Vec<u8> {
        let mut b = vec![b'R'];
        b.extend(rel.id.to_be_bytes());
        b.extend(rel.namespace.as_bytes());
        b.push(0);
        b.extend(rel.name.as_bytes());
        b.push(0);
        b.push(rel.replica_identity);
        b.extend((rel.columns.len() as i16).to_be_bytes());
        for c in &rel.columns {
            b.push(u8::from(c.key));
            b.extend(c.name.as_bytes());
            b.push(0);
            b.extend(c.type_oid.to_be_bytes());
            b.extend((-1_i32).to_be_bytes());
        }
        b
    }

    /// Encodes tuple data, for tests.
    pub(crate) fn tuple_bytes(values: &[Option<&str>]) -> Vec<u8> {
        let mut b = (values.len() as i16).to_be_bytes().to_vec();
        for v in values {
            match v {
                None => b.push(b'n'),
                Some(t) => {
                    b.push(b't');
                    b.extend((t.len() as i32).to_be_bytes());
                    b.extend(t.as_bytes());
                }
            }
        }
        b
    }

    fn text(values: &[&str]) -> Tuple {
        values
            .iter()
            .map(|v| Datum::Text(v.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn decodes_relation() {
        let rel = Relation {
            id: 16385,
            namespace: "knotrel_graph".into(),
            name: "edges".into(),
            replica_identity: b'd',
            columns: vec![
                Column {
                    key: true,
                    name: "source".into(),
                    type_oid: 20,
                },
                Column {
                    key: false,
                    name: "enabled".into(),
                    type_oid: 16,
                },
            ],
        };
        assert_eq!(
            decode(&relation_bytes(&rel)).unwrap(),
            Message::Relation(rel)
        );
    }

    #[test]
    fn decodes_insert_update_delete() {
        let mut ins = vec![b'I'];
        ins.extend(7_u32.to_be_bytes());
        ins.push(b'N');
        ins.extend(tuple_bytes(&[Some("5"), Some("t")]));
        assert_eq!(
            decode(&ins).unwrap(),
            Message::Insert {
                relation: 7,
                new: text(&["5", "t"])
            }
        );

        let mut upd = vec![b'U'];
        upd.extend(7_u32.to_be_bytes());
        upd.push(b'K');
        upd.extend(tuple_bytes(&[Some("4"), None]));
        upd.push(b'N');
        upd.extend(tuple_bytes(&[Some("5"), Some("f")]));
        assert_eq!(
            decode(&upd).unwrap(),
            Message::Update {
                relation: 7,
                old: Some(vec![Datum::Text(b"4".to_vec()), Datum::Null]),
                new: text(&["5", "f"])
            }
        );

        let mut upd_plain = vec![b'U'];
        upd_plain.extend(7_u32.to_be_bytes());
        upd_plain.push(b'N');
        upd_plain.extend(tuple_bytes(&[Some("5"), Some("f")]));
        assert!(matches!(
            decode(&upd_plain).unwrap(),
            Message::Update { old: None, .. }
        ));

        let mut del = vec![b'D'];
        del.extend(7_u32.to_be_bytes());
        del.push(b'K');
        del.extend(tuple_bytes(&[Some("5"), None]));
        assert!(matches!(
            decode(&del).unwrap(),
            Message::Delete { relation: 7, .. }
        ));
    }

    #[test]
    fn decodes_truncate_and_ignores_others() {
        let mut t = vec![b'T'];
        t.extend(2_u32.to_be_bytes());
        t.push(0);
        t.extend(7_u32.to_be_bytes());
        t.extend(8_u32.to_be_bytes());
        assert_eq!(
            decode(&t).unwrap(),
            Message::Truncate {
                relations: vec![7, 8]
            }
        );
        assert_eq!(decode(b"Yanything").unwrap(), Message::Ignored(b'Y'));
    }

    #[test]
    fn rejects_malformed_messages() {
        assert!(decode(b"").is_err());
        assert!(decode(b"Z").is_err());
        let mut ins = vec![b'I'];
        ins.extend(7_u32.to_be_bytes());
        ins.push(b'N');
        ins.extend(2_i16.to_be_bytes());
        ins.push(b't');
        ins.extend(10_i32.to_be_bytes());
        ins.extend(b"short");
        assert!(decode(&ins).is_err(), "truncated value");
        let mut bin = vec![b'I'];
        bin.extend(7_u32.to_be_bytes());
        bin.push(b'N');
        bin.extend(1_i16.to_be_bytes());
        bin.push(b'b');
        assert!(decode(&bin).is_err(), "binary format");
        let mut trailing = vec![b'D'];
        trailing.extend(7_u32.to_be_bytes());
        trailing.push(b'K');
        trailing.extend(tuple_bytes(&[Some("1")]));
        trailing.push(0);
        assert!(decode(&trailing).is_err(), "trailing bytes");
    }
}
