-- With allow_recreate, a plain column becoming generated is dropped and added again.
-- recreate
-- no_reverse
-- expect: DROP COLUMN "area"
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (w integer, area integer);
-- @b
CREATE TABLE app.t (w integer, area integer GENERATED ALWAYS AS (w * 2) STORED);
