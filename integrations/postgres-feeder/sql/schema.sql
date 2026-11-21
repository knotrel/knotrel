-- Reference graph schema for the Knotrel PostgreSQL feeder.
-- Run as the role that will own the tables (the application role), not as the
-- feeder role. Mapping and constraints: docs/integrations/postgresql-feeder.md,
-- section 4.

CREATE SCHEMA IF NOT EXISTS knotrel_graph;

-- A node. id is the Knotrel node ID (0 to 2^63 - 1); active is its
-- lifecycle flag.
CREATE TABLE IF NOT EXISTS knotrel_graph.nodes (
    id     bigint  PRIMARY KEY CHECK (id >= 0),
    active boolean NOT NULL DEFAULT true
);

-- An undirected edge, stored once with source < target (this also excludes
-- self-loops). enabled is its lifecycle flag. Deleting a node deletes its
-- edges, as Knotrel's remove_node does.
CREATE TABLE IF NOT EXISTS knotrel_graph.edges (
    source  bigint  NOT NULL REFERENCES knotrel_graph.nodes (id) ON DELETE CASCADE,
    target  bigint  NOT NULL REFERENCES knotrel_graph.nodes (id) ON DELETE CASCADE,
    enabled boolean NOT NULL DEFAULT true,
    PRIMARY KEY (source, target),
    CHECK (source < target)
);

CREATE INDEX IF NOT EXISTS edges_target ON knotrel_graph.edges (target);
