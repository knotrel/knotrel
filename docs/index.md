---
layout: home
title: Knotrel Documentation
body_class: home
release: v0.8.0
---
<p class="eyebrow">Architecture</p>

## Where Knotrel fits

<img class="architecture-light" src="diagrams/architecture.svg" alt="Source-managed Knotrel architecture">
<img class="architecture-dark" src="diagrams/architecture-dark.svg" alt="Source-managed Knotrel architecture">

The authoritative source and feeder are external. In source-managed mode, the feeder imports a graph and delivers ordered updates; Knotrel answers queries against its in-memory state. A [reference PostgreSQL feeder](integrations/postgresql-feeder.md) (logical replication, unreleased, planned for v0.9.0) shows a complete integration. In standalone mode, clients operate the graph directly. [Architecture details](architecture-site.md).

## Good to know

- **Volatile by design.** The graph lives in memory. After a restart, rebuild or resynchronize from the authoritative source.
- **No built-in authentication.** Place Knotrel behind a trusted boundary or gateway; see the [deployment guide](deployment.md).
{: .callouts}
