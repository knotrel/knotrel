# PostgreSQL reference integration (Compose example)

PostgreSQL with a demo graph, Knotrel in source-managed mode, the reference
PostgreSQL feeder and the console, on one private Compose network. Only the
console is published, on the host's loopback.

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

- `generate-secrets.sh` writes random passwords into `secrets/` (ignored by
  git); Compose passes them as secrets.
- `initdb/10-knotrel.sh` creates the schema, the publication, the
  least-privileged feeder role and the demo data on the first start, from the
  SQL files in [`integrations/postgres-feeder/sql/`](../../integrations/postgres-feeder/sql/schema.sql).
- No TLS: a local example. For TLS and authentication in front of Knotrel and
  the console, see [`examples/gateway/`](../gateway/compose.yaml).

The feeder itself (code, configuration, tests, supported PostgreSQL versions):
[`integrations/postgres-feeder/`](../../integrations/postgres-feeder/README.md).
Design, guarantees and limits:
[docs/integrations/postgresql-feeder.md](../../docs/integrations/postgresql-feeder.md).
