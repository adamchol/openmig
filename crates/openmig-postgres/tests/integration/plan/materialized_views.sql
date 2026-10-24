-- Materialized views: recreated (with indexes) when the query changes; indexes, storage
-- parameters and population change in place.
-- expect: REFRESH MATERIALIZED VIEW "app"."pop"
-- @both
CREATE SCHEMA app;
CREATE TABLE app.t (id integer PRIMARY KEY, n integer);
-- @a
CREATE MATERIALIZED VIEW app.m AS SELECT id, n FROM app.t;
CREATE UNIQUE INDEX m_id_idx ON app.m (id);
CREATE MATERIALIZED VIEW app.pop AS SELECT id FROM app.t WITH NO DATA;
CREATE MATERIALIZED VIEW app.idx AS SELECT id, n FROM app.t;
-- @b
CREATE MATERIALIZED VIEW app.m AS SELECT id, n, n * 2 AS d FROM app.t;
CREATE UNIQUE INDEX m_id_idx ON app.m (id);
CREATE MATERIALIZED VIEW app.pop WITH (fillfactor = 50) AS SELECT id FROM app.t;
CREATE MATERIALIZED VIEW app.idx AS SELECT id, n FROM app.t;
CREATE INDEX idx_n ON app.idx (n);
COMMENT ON MATERIALIZED VIEW app.idx IS 'indexed';
