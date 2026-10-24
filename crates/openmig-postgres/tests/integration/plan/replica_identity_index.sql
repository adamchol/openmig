-- Recreating the index a table uses as its replica identity sets the replica identity again.
-- expect: REPLICA IDENTITY USING INDEX "t_k_idx"
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (id integer NOT NULL, k integer NOT NULL);
CREATE UNIQUE INDEX t_k_idx ON app.t (k);
ALTER TABLE app.t REPLICA IDENTITY USING INDEX t_k_idx;
-- @b
CREATE TABLE app.t (id integer NOT NULL, k integer NOT NULL);
CREATE UNIQUE INDEX t_k_idx ON app.t (k, id);
ALTER TABLE app.t REPLICA IDENTITY USING INDEX t_k_idx;
