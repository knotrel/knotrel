-- Demo data: a 20 x 20 grid with a few inactive nodes and disabled edges.
INSERT INTO knotrel_graph.nodes
SELECT r * 20 + c, (r * 20 + c) % 37 <> 0
FROM generate_series(0, 19) r, generate_series(0, 19) c;

INSERT INTO knotrel_graph.edges
SELECT r * 20 + c, r * 20 + c + 1, (r + c) % 11 <> 0
FROM generate_series(0, 19) r, generate_series(0, 18) c
UNION ALL
SELECT r * 20 + c, (r + 1) * 20 + c, (r * c) % 13 <> 1
FROM generate_series(0, 18) r, generate_series(0, 19) c;
