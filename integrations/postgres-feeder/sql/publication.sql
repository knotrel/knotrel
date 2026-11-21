-- The publication the feeder reads. Run as the owner of the tables. It must
-- contain exactly the two graph tables, publish every change kind, and have no
-- row filter or column list (the feeder checks this at startup).
CREATE PUBLICATION knotrel_graph
    FOR TABLE knotrel_graph.nodes, knotrel_graph.edges
    WITH (publish = 'insert, update, delete, truncate');
