#!/bin/sh
# First start of the demo database: schema, publication, feeder role and demo
# data. Runs inside the postgres container (docker-entrypoint-initdb.d).
set -eu
psql -v ON_ERROR_STOP=1 -U postgres -d app -q -f /knotrel-sql/schema.sql
psql -v ON_ERROR_STOP=1 -U postgres -d app -q -f /knotrel-sql/publication.sql
psql -v ON_ERROR_STOP=1 -U postgres -d app -q \
  -v feeder_password="$(cat /run/secrets/feeder_password)" -f /knotrel-sql/roles.sql
psql -v ON_ERROR_STOP=1 -U postgres -d app -q -f /knotrel-sql/demo-data.sql
