-- Objects that take over a name another kind of object had: dropped first.
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.x (id integer);
CREATE TABLE app.t (id integer, k integer);
CREATE INDEX t_k_key ON app.t (k);
CREATE VIEW app.y AS SELECT 1 AS one;
-- @b
CREATE VIEW app.x AS SELECT 1 AS id;
CREATE TABLE app.t (id integer, k integer CONSTRAINT t_k_key UNIQUE);
CREATE TABLE app.y (one integer);
