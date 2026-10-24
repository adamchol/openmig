-- Unlogged tables, storage parameters, replica identity, column storage and compression.
-- expect: SET UNLOGGED
-- expect: REPLICA IDENTITY USING INDEX "t_k_key"
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (id integer PRIMARY KEY, k integer NOT NULL UNIQUE, body text, n numeric) WITH (fillfactor = 70, autovacuum_enabled = false);
CREATE TABLE app.u (id integer);
ALTER TABLE app.u REPLICA IDENTITY FULL;
ALTER TABLE app.t ALTER COLUMN body SET STORAGE EXTERNAL;
-- @b
CREATE UNLOGGED TABLE app.t (id integer PRIMARY KEY, k integer NOT NULL UNIQUE, body text COMPRESSION pglz, n numeric) WITH (fillfactor = 80);
ALTER TABLE app.t REPLICA IDENTITY USING INDEX t_k_key;
ALTER TABLE app.t ALTER COLUMN n SET STORAGE PLAIN;
CREATE TABLE app.u (id integer);
ALTER TABLE app.u REPLICA IDENTITY NOTHING;
