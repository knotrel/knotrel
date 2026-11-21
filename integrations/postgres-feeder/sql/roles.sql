-- The feeder role: replication and read access to the two graph tables,
-- nothing else. It cannot write, does not own the tables and does not create
-- the publication. Run as a superuser (or a role with CREATEROLE) with psql:
--
--   psql -v feeder_password="$(cat feeder_password)" -f roles.sql
--
-- The password is passed as a psql variable so it is never stored in this
-- repository or in shell history.

CREATE ROLE knotrel_feeder LOGIN REPLICATION PASSWORD :'feeder_password';
GRANT CONNECT ON DATABASE :"DBNAME" TO knotrel_feeder;
GRANT USAGE ON SCHEMA knotrel_graph TO knotrel_feeder;
GRANT SELECT ON knotrel_graph.nodes, knotrel_graph.edges TO knotrel_feeder;
