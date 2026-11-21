# Knotrel PostgreSQL reference feeder

A reference integration (roadmap 0.9.1 and 0.9.2): PostgreSQL stays the
authority, an external feeder reads two tables with logical replication and
keeps a Knotrel server in source-managed mode synchronized. It is an example
with stated limits, not a general PostgreSQL connector, and nothing here runs
inside the Knotrel server.

**Read first:** [design, guarantees and correctness criteria](../../docs/integrations/postgresql-feeder.md).
In short: no lost update between snapshot and stream, progress confirmed to
PostgreSQL only after Knotrel applied it, a full rebuild after every Knotrel
restart, bounded WAL retention, and **no PostgreSQL transaction atomicity in
Knotrel** (a reader can see a partly applied transaction; the source position
tells whether it did).

This directory is its own Cargo workspace with its own lockfile. Building
Knotrel never builds the feeder.

## Supported versions

PostgreSQL 15, 16, 17, 18 and 19 (tested in CI on each), `wal_level =
logical`, finite `max_slot_wal_keep_size`. Knotrel 0.8 or later with
`KNOTREL_MODE=source-managed` and `KNOTREL_TOPOLOGY_LIFECYCLE=true`.

## Try it with Compose

PostgreSQL with a demo graph, Knotrel, the feeder and the console:

```bash
examples/postgres-feeder/generate-secrets.sh
docker compose -f examples/postgres-feeder/compose.yaml up --build
```

Open <http://127.0.0.1:7380/>: the Overview shows the synchronization state,
the Browser the 400 demo nodes. Change the source and watch Knotrel follow:

```bash
docker compose -f examples/postgres-feeder/compose.yaml exec postgres \
  psql -U postgres -d app -c "UPDATE knotrel_graph.nodes SET active = false WHERE id = 21"
```

The Compose example lives in [`examples/postgres-feeder/`](../../examples/postgres-feeder/README.md).
The passwords are random files in its `secrets/` directory (ignored by git) passed as
Compose secrets. Only the console port is published, on loopback.

## Set it up on your database

1. PostgreSQL settings: `wal_level = logical` (restart required),
   `max_slot_wal_keep_size` (for example `1GB`), and room for one more
   replication slot and WAL sender.
2. As the table owner: [`sql/schema.sql`](sql/schema.sql) (or your tables with
   the same columns and constraints) and [`sql/publication.sql`](sql/publication.sql).
3. As a superuser: [`sql/roles.sql`](sql/roles.sql) creates the feeder role with
   `LOGIN REPLICATION` and `SELECT` on the two tables only.
4. `knotrel-pg-feeder check` validates everything, then `knotrel-pg-feeder`
   runs.

## Configuration

Environment variables only (the password never goes on the command line):

| Variable | Default | Meaning |
| --- | --- | --- |
| `KNOTREL_PG_HOST`, `KNOTREL_PG_PORT` | `127.0.0.1`, `5432` | PostgreSQL server |
| `KNOTREL_PG_USER` | required | feeder role |
| `KNOTREL_PG_PASSWORD_FILE` | — | file with the password (preferred) |
| `KNOTREL_PG_PASSWORD` | — | the password, if no file |
| `KNOTREL_PG_DATABASE` | required | database |
| `KNOTREL_PG_PUBLICATION` | `knotrel_graph` | publication |
| `KNOTREL_PG_SCHEMA`, `KNOTREL_PG_NODES_TABLE`, `KNOTREL_PG_EDGES_TABLE` | `knotrel_graph`, `nodes`, `edges` | graph tables |
| `KNOTREL_PG_SLOT_PREFIX` | `knotrel_feeder` | prefix reserved for this feeder's slots |
| `KNOTREL_URL` | `http://127.0.0.1:8080` | Knotrel server |
| `KNOTREL_FEEDER_LEASE_MS` | `10000` | feeder lease (1000 to 60000) |
| `KNOTREL_FEEDER_MAX_BATCH` | `1024` | events per request (1 to 1024) |
| `KNOTREL_FEEDER_CATCH_UP_IDLE_MS` | `500` | idle time that counts as caught up |
| `KNOTREL_FEEDER_SLOT_CHECK_SECS` | `30` | slot retention check and cleanup interval |
| `KNOTREL_FEEDER_ALLOW_UNBOUNDED_WAL` | `false` | accept `max_slot_wal_keep_size = -1` |
| `KNOTREL_FEEDER_REQUEST_TIMEOUT_MS`, `KNOTREL_FEEDER_IMPORT_TIMEOUT_MS` | `10000`, `300000` | Knotrel request timeouts |
| `RUST_LOG` | `info` | log level |

## Commands and exit codes

- `knotrel-pg-feeder [run]` synchronizes until stopped (SIGTERM or Ctrl-C).
- `knotrel-pg-feeder check` runs the startup checks and exits.
- `knotrel-pg-feeder drop-slots` drops this feeder's inactive slots. Use it to
  decommission a feeder, and in the recovery from an unsupported schema
  change (design document, section 4.3).

| Exit code | Meaning |
| --- | --- |
| 0 | stopped on request |
| 1 | unexpected error |
| 2 | configuration or server settings (version, `wal_level`, unbounded retention, Knotrel mode) |
| 3 | unsupported schema, schema change or value |
| 4 | Knotrel capacity (graph limits, import body limit) |
| 5 | another feeder took over the Knotrel server |

## Tests

The integration tests run against a real PostgreSQL server and a real
`knotrel-server`:

```bash
cargo build --bin knotrel-server                       # at the repository root
cd integrations/postgres-feeder
export KNOTREL_PG_ADMIN_URL="$(scripts/start-test-postgres.sh 18)"
KNOTREL_PG_TESTS_REQUIRED=1 cargo test
scripts/stop-test-postgres.sh 18
```

`start-test-postgres.sh` accepts 15 to 19. The upgrade test
(`tests/upgrade.rs`) also needs `KNOTREL_PREVIOUS_BIN`, a `knotrel-server` built
from the previous release tag; CI runs it on PostgreSQL 18. Without `KNOTREL_PG_ADMIN_URL` the
integration tests are skipped (CI sets `KNOTREL_PG_TESTS_REQUIRED=1`, so they
cannot be skipped there). Each test uses its own database, role and slot
prefix. Feeder logs are kept in `target/feeder-test-logs/`.
