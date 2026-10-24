-- Changing how a table is partitioned is unsupported.
-- unsupported
-- no_reverse
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (id integer, at date) PARTITION BY RANGE (at);
-- @b
CREATE TABLE app.t (id integer, at date) PARTITION BY LIST (id);
