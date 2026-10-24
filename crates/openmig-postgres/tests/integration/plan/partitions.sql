-- Partitions: new partitions, dropped partitions, a bound change through detach and attach,
-- and an index on the partitioned table.
-- expect: DETACH PARTITION "app"."p2"
-- @both
CREATE SCHEMA app;
CREATE TABLE app.events (id bigint NOT NULL, at date NOT NULL, payload text) PARTITION BY RANGE (at);
-- @a
CREATE TABLE app.p1 PARTITION OF app.events FOR VALUES FROM ('2026-01-01') TO ('2026-02-01');
CREATE TABLE app.p2 PARTITION OF app.events FOR VALUES FROM ('2026-02-01') TO ('2026-03-01');
CREATE TABLE app.old PARTITION OF app.events FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');
-- @b
CREATE TABLE app.p1 PARTITION OF app.events FOR VALUES FROM ('2026-01-01') TO ('2026-02-01');
CREATE TABLE app.p2 PARTITION OF app.events FOR VALUES FROM ('2026-02-01') TO ('2026-04-01');
CREATE TABLE app.rest PARTITION OF app.events DEFAULT;
CREATE INDEX events_at_idx ON app.events (at);
ALTER TABLE app.events ADD PRIMARY KEY (id, at);
