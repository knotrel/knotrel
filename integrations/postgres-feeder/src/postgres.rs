//! The normal (non-replication) PostgreSQL connection: preflight checks,
//! schema validation, slot management and snapshot reads.

use crate::mapping::TableNames;
use crate::position::is_valid_slot_name;
use std::fmt;
use tokio_postgres::{Client, NoTls};

/// Lowest supported `server_version_num` (PostgreSQL 15).
pub const MIN_SERVER_VERSION: i32 = 150_000;
/// First unsupported `server_version_num` (PostgreSQL 20).
pub const MAX_SERVER_VERSION_EXCLUSIVE: i32 = 200_000;

/// Connection parameters. `Debug` never prints the password.
#[derive(Clone)]
pub struct PgParams {
    /// Host name or address.
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// Role name.
    pub user: String,
    /// Password (never logged).
    pub password: String,
    /// Database name.
    pub database: String,
}

impl fmt::Debug for PgParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "postgres://{}@{}:{}/{}",
            self.user, self.host, self.port, self.database
        )
    }
}

/// A failed PostgreSQL interaction.
#[derive(Debug)]
pub enum PgError {
    /// The connection failed or broke; retrying may help.
    Connection(String),
    /// The server is not usable as configured (version, settings, schema,
    /// publication). Retrying does not help.
    Unsupported(String),
}

impl fmt::Display for PgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(m) => write!(f, "PostgreSQL connection error: {m}"),
            Self::Unsupported(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for PgError {}

fn connection(e: tokio_postgres::Error) -> PgError {
    // `tokio_postgres::Error`'s Display never includes the password.
    match e.as_db_error() {
        Some(db) => PgError::Connection(format!("{} ({})", db.message(), db.code().code())),
        None => PgError::Connection(e.to_string()),
    }
}

/// Quotes an SQL identifier.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A replication slot as listed in `pg_replication_slots`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotInfo {
    /// Slot name.
    pub name: String,
    /// Whether a process currently streams from it.
    pub active: bool,
    /// Output plugin.
    pub plugin: Option<String>,
    /// Database the slot belongs to.
    pub database: Option<String>,
    /// `wal_status` (`reserved`, `extended`, `unreserved`, `lost`).
    pub wal_status: Option<String>,
    /// WAL retained by the slot, in bytes, if known.
    pub retained_bytes: Option<i64>,
}

impl SlotInfo {
    /// Whether the slot can still stream (its WAL was not removed).
    pub fn usable(&self) -> bool {
        self.wal_status.as_deref() != Some("lost")
    }
}

/// The control connection.
pub struct Control {
    client: Client,
    params: PgParams,
}

impl Control {
    /// Connects with the given parameters.
    ///
    /// # Errors
    ///
    /// [`PgError::Connection`] if the server cannot be reached or rejects
    /// the login.
    pub async fn connect(params: &PgParams) -> Result<Self, PgError> {
        let mut config = tokio_postgres::Config::new();
        config
            .host(&params.host)
            .port(params.port)
            .user(&params.user)
            .password(&params.password)
            .dbname(&params.database)
            .application_name("knotrel-pg-feeder")
            .connect_timeout(std::time::Duration::from_secs(10));
        let (client, conn) = config.connect(NoTls).await.map_err(connection)?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                tracing::debug!(error = %e, "control connection closed");
            }
        });
        Ok(Self {
            client,
            params: params.clone(),
        })
    }

    /// Whether the connection is closed.
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    /// Checks the server version and the replication settings.
    ///
    /// # Errors
    ///
    /// [`PgError::Unsupported`] for an unsupported version, `wal_level` other
    /// than `logical`, or unbounded `max_slot_wal_keep_size` without the
    /// override.
    pub async fn check_server(&self, allow_unbounded_wal: bool) -> Result<i32, PgError> {
        let row = self
            .client
            .query_one(
                "SELECT current_setting('server_version_num')::int4, \
                        current_setting('wal_level'), \
                        current_setting('max_slot_wal_keep_size')",
                &[],
            )
            .await
            .map_err(connection)?;
        let version: i32 = row.get(0);
        let wal_level: String = row.get(1);
        let keep: String = row.get(2);
        if !(MIN_SERVER_VERSION..MAX_SERVER_VERSION_EXCLUSIVE).contains(&version) {
            return Err(PgError::Unsupported(format!(
                "PostgreSQL server_version_num {version} is not supported (supported: 15 to 19)"
            )));
        }
        if wal_level != "logical" {
            return Err(PgError::Unsupported(format!(
                "wal_level is {wal_level:?}; logical replication needs wal_level = logical"
            )));
        }
        if keep == "-1" {
            if allow_unbounded_wal {
                tracing::warn!(
                    "max_slot_wal_keep_size is -1: a stalled feeder can retain WAL without bound"
                );
            } else {
                return Err(PgError::Unsupported(
                    "max_slot_wal_keep_size is -1 (unbounded): set a finite value so a stalled \
                     feeder cannot fill the disk, or set KNOTREL_FEEDER_ALLOW_UNBOUNDED_WAL=true"
                        .into(),
                ));
            }
        }
        Ok(version)
    }

    /// Validates the graph tables and the publication (design document,
    /// section 4.3).
    ///
    /// # Errors
    ///
    /// [`PgError::Unsupported`] naming the first problem found.
    pub async fn validate_schema(
        &self,
        names: &TableNames,
        publication: &str,
    ) -> Result<(), PgError> {
        let nodes = self
            .table(
                names,
                &names.nodes,
                &[("id", "int8"), ("active", "bool")],
                &["id"],
                "(id >= 0)",
            )
            .await?;
        let edges = self
            .table(
                names,
                &names.edges,
                &[("source", "int8"), ("target", "int8"), ("enabled", "bool")],
                &["source", "target"],
                "(source < target)",
            )
            .await?;

        let publication_row = self
            .client
            .query_opt(
                "SELECT pubinsert, pubupdate, pubdelete, pubtruncate, puballtables \
                 FROM pg_publication WHERE pubname = $1",
                &[&publication],
            )
            .await
            .map_err(connection)?
            .ok_or_else(|| {
                PgError::Unsupported(format!(
                    "publication {publication:?} does not exist (create it as the table owner, \
                     see sql/publication.sql)"
                ))
            })?;
        let flags: [bool; 4] = [
            publication_row.get(0),
            publication_row.get(1),
            publication_row.get(2),
            publication_row.get(3),
        ];
        if flags != [true; 4] {
            return Err(PgError::Unsupported(format!(
                "publication {publication:?} must publish insert, update, delete and truncate"
            )));
        }
        if publication_row.get::<_, bool>(4) {
            return Err(PgError::Unsupported(format!(
                "publication {publication:?} is FOR ALL TABLES; it must contain exactly the two \
                 graph tables"
            )));
        }
        let rows = self
            .client
            .query(
                "SELECT schemaname::text, tablename::text, rowfilter IS NULL, \
                        coalesce(cardinality(attnames), 0) \
                 FROM pg_publication_tables WHERE pubname = $1",
                &[&publication],
            )
            .await
            .map_err(connection)?;
        let mut seen = Vec::new();
        for row in &rows {
            let schema: String = row.get(0);
            let table: String = row.get(1);
            let no_filter: bool = row.get(2);
            let published_columns: i32 = row.get(3);
            let expected_columns = if table == names.nodes { nodes } else { edges };
            if schema != names.schema || (table != names.nodes && table != names.edges) {
                return Err(PgError::Unsupported(format!(
                    "publication {publication:?} contains {schema}.{table}, which is not a graph \
                     table"
                )));
            }
            if !no_filter {
                return Err(PgError::Unsupported(format!(
                    "publication {publication:?} has a row filter on {schema}.{table}"
                )));
            }
            if i64::from(published_columns) != expected_columns {
                return Err(PgError::Unsupported(format!(
                    "publication {publication:?} has a column list on {schema}.{table}"
                )));
            }
            seen.push(table);
        }
        seen.sort();
        let mut expected = vec![names.nodes.clone(), names.edges.clone()];
        expected.sort();
        if seen != expected {
            return Err(PgError::Unsupported(format!(
                "publication {publication:?} must contain exactly {}.{} and {}.{}",
                names.schema, names.nodes, names.schema, names.edges
            )));
        }
        Ok(())
    }

    /// Checks one table; returns its number of columns.
    async fn table(
        &self,
        names: &TableNames,
        table: &str,
        columns: &[(&str, &str)],
        primary_key: &[&str],
        check: &str,
    ) -> Result<i64, PgError> {
        let qualified = format!("{}.{}", names.schema, table);
        let row = self
            .client
            .query_opt(
                "SELECT c.oid::int8, c.relreplident::text \
                 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1 AND c.relname = $2 AND c.relkind IN ('r', 'p')",
                &[&names.schema, &table],
            )
            .await
            .map_err(connection)?
            .ok_or_else(|| PgError::Unsupported(format!("table {qualified} does not exist")))?;
        let oid: i64 = row.get(0);
        let replident: String = row.get(1);
        if replident != "d" && replident != "f" {
            return Err(PgError::Unsupported(format!(
                "{qualified}: replica identity must be DEFAULT or FULL"
            )));
        }
        let attributes = self
            .client
            .query(
                "SELECT attname::text, format_type(atttypid, NULL), atttypid::regtype::text, attnotnull \
                 FROM pg_attribute WHERE attrelid = $1::int8::oid AND attnum > 0 AND NOT attisdropped",
                &[&oid],
            )
            .await
            .map_err(connection)?;
        for (name, type_name) in columns {
            let attribute = attributes
                .iter()
                .find(|a| a.get::<_, String>(0) == *name)
                .ok_or_else(|| {
                    PgError::Unsupported(format!(
                        "{qualified}: required column {name:?} is missing"
                    ))
                })?;
            let actual: String = attribute.get(2);
            let expected = if *type_name == "int8" {
                "bigint"
            } else {
                "boolean"
            };
            if actual != expected {
                return Err(PgError::Unsupported(format!(
                    "{qualified}: column {name:?} has type {actual}, expected {expected}"
                )));
            }
            if !attribute.get::<_, bool>(3) {
                return Err(PgError::Unsupported(format!(
                    "{qualified}: column {name:?} must be NOT NULL"
                )));
            }
        }
        let key_rows = self
            .client
            .query(
                "SELECT a.attname::text FROM pg_index i \
                 JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY (i.indkey) \
                 WHERE i.indrelid = $1::int8::oid AND i.indisprimary",
                &[&oid],
            )
            .await
            .map_err(connection)?;
        let mut key: Vec<String> = key_rows.iter().map(|r| r.get(0)).collect();
        key.sort();
        let mut expected_key: Vec<String> = primary_key.iter().map(|s| s.to_string()).collect();
        expected_key.sort();
        if key != expected_key {
            return Err(PgError::Unsupported(format!(
                "{qualified}: primary key must be ({}), found ({})",
                primary_key.join(", "),
                key.join(", ")
            )));
        }
        let checks = self
            .client
            .query(
                "SELECT pg_get_constraintdef(oid) FROM pg_constraint \
                 WHERE conrelid = $1::int8::oid AND contype = 'c'",
                &[&oid],
            )
            .await
            .map_err(connection)?;
        let normalized_check: String = check.split_whitespace().collect();
        let found = checks.iter().any(|r| {
            let def: String = r.get(0);
            def.split_whitespace()
                .collect::<String>()
                .contains(&normalized_check)
        });
        if !found {
            return Err(PgError::Unsupported(format!(
                "{qualified}: missing CHECK {check} (see sql/schema.sql)"
            )));
        }
        Ok(attributes.len() as i64)
    }

    /// Reads the complete graph in the exported snapshot `snapshot`.
    ///
    /// # Errors
    ///
    /// [`PgError::Connection`] on connection errors or if the snapshot is no
    /// longer valid; [`PgError::Unsupported`] for a value outside the
    /// mapping (negative ID, self-loop).
    #[allow(clippy::type_complexity)]
    pub async fn read_image(
        &self,
        names: &TableNames,
        snapshot: &str,
    ) -> Result<(Vec<(u64, bool)>, Vec<(u64, u64, bool)>), PgError> {
        if snapshot.is_empty() || !snapshot.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return Err(PgError::Connection(format!(
                "unexpected snapshot name {snapshot:?}"
            )));
        }
        let schema = quote_ident(&names.schema);
        self.client
            .batch_execute(&format!(
                "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY; \
                 SET TRANSACTION SNAPSHOT '{snapshot}';"
            ))
            .await
            .map_err(connection)?;
        let result = async {
            let node_rows = self
                .client
                .query(
                    &format!(
                        "SELECT id, active FROM {schema}.{} ORDER BY id",
                        quote_ident(&names.nodes)
                    ),
                    &[],
                )
                .await
                .map_err(connection)?;
            let edge_rows = self
                .client
                .query(
                    &format!(
                        "SELECT source, target, enabled FROM {schema}.{} ORDER BY source, target",
                        quote_ident(&names.edges)
                    ),
                    &[],
                )
                .await
                .map_err(connection)?;
            let id = |v: i64, what: &str| {
                u64::try_from(v).map_err(|_| {
                    PgError::Unsupported(format!("unsupported value: {what} = {v} is negative"))
                })
            };
            let mut nodes = Vec::with_capacity(node_rows.len());
            for r in &node_rows {
                nodes.push((id(r.get(0), "nodes.id")?, r.get(1)));
            }
            let mut edges = Vec::with_capacity(edge_rows.len());
            for r in &edge_rows {
                let s = id(r.get(0), "edges.source")?;
                let t = id(r.get(1), "edges.target")?;
                if s == t {
                    return Err(PgError::Unsupported(format!(
                        "unsupported value: self-loop on node {s}"
                    )));
                }
                edges.push((s, t, r.get(2)));
            }
            Ok((nodes, edges))
        }
        .await;
        let end = self
            .client
            .batch_execute("COMMIT")
            .await
            .map_err(connection);
        let image = result?;
        end?;
        Ok(image)
    }

    /// The current WAL write position (`pg_current_wal_lsn()`).
    ///
    /// # Errors
    ///
    /// [`PgError::Connection`] on connection errors.
    pub async fn current_wal_lsn(&self) -> Result<pgwire_replication::Lsn, PgError> {
        let row = self
            .client
            .query_one("SELECT pg_current_wal_lsn()::text", &[])
            .await
            .map_err(connection)?;
        let text: String = row.get(0);
        pgwire_replication::Lsn::parse(&text).map_err(|e| PgError::Connection(e.to_string()))
    }

    /// Lists the logical slots whose name starts with `<prefix>_`.
    ///
    /// # Errors
    ///
    /// [`PgError::Connection`] on connection errors.
    pub async fn slots(&self, prefix: &str) -> Result<Vec<SlotInfo>, PgError> {
        let pattern = format!("{prefix}_");
        let rows = self
            .client
            .query(
                "SELECT slot_name::text, active, plugin::text, database::text, wal_status, \
                        pg_wal_lsn_diff(pg_current_wal_lsn(), confirmed_flush_lsn)::int8 \
                 FROM pg_replication_slots \
                 WHERE slot_type = 'logical' AND left(slot_name::text, length($1)) = $1 \
                 ORDER BY slot_name",
                &[&pattern],
            )
            .await
            .map_err(connection)?;
        Ok(rows
            .iter()
            .map(|r| SlotInfo {
                name: r.get(0),
                active: r.get(1),
                plugin: r.get(2),
                database: r.get(3),
                wal_status: r.get(4),
                retained_bytes: r.get(5),
            })
            .collect())
    }

    /// Looks up one slot by name (any prefix).
    ///
    /// # Errors
    ///
    /// [`PgError::Connection`] on connection errors.
    pub async fn slot(&self, name: &str) -> Result<Option<SlotInfo>, PgError> {
        let row = self
            .client
            .query_opt(
                "SELECT slot_name::text, active, plugin::text, database::text, wal_status, \
                        pg_wal_lsn_diff(pg_current_wal_lsn(), confirmed_flush_lsn)::int8 \
                 FROM pg_replication_slots WHERE slot_name = $1",
                &[&name],
            )
            .await
            .map_err(connection)?;
        Ok(row.map(|r| SlotInfo {
            name: r.get(0),
            active: r.get(1),
            plugin: r.get(2),
            database: r.get(3),
            wal_status: r.get(4),
            retained_bytes: r.get(5),
        }))
    }

    /// Whether `slot` is a slot this feeder may use: logical, `pgoutput`, in
    /// this database.
    pub fn owns(&self, slot: &SlotInfo, prefix: &str) -> bool {
        slot.name.starts_with(&format!("{prefix}_"))
            && slot.plugin.as_deref() == Some("pgoutput")
            && slot.database.as_deref() == Some(self.params.database.as_str())
    }

    /// Drops this feeder's inactive slots other than `keep`. Never touches a
    /// slot without the prefix, of another database, or in use.
    ///
    /// Returns the names of the dropped slots.
    ///
    /// # Errors
    ///
    /// [`PgError::Connection`] on connection errors. A slot that becomes
    /// active between the listing and the drop is skipped.
    pub async fn drop_own_slots(
        &self,
        prefix: &str,
        keep: Option<&str>,
    ) -> Result<Vec<String>, PgError> {
        let mut dropped = Vec::new();
        for slot in self.slots(prefix).await? {
            if Some(slot.name.as_str()) == keep || slot.active || !self.owns(&slot, prefix) {
                continue;
            }
            if !is_valid_slot_name(&slot.name) {
                continue;
            }
            match self
                .client
                .execute("SELECT pg_drop_replication_slot($1)", &[&slot.name])
                .await
            {
                Ok(_) => {
                    tracing::info!(slot = %slot.name, "dropped feeder slot");
                    dropped.push(slot.name);
                }
                Err(e) => {
                    tracing::warn!(slot = %slot.name, error = %connection(e), "could not drop slot; skipped");
                }
            }
        }
        Ok(dropped)
    }
}
