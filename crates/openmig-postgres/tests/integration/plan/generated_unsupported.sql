-- A plain column cannot become a generated column.
-- unsupported
-- no_reverse
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (w integer, area integer);
-- @b
CREATE TABLE app.t (w integer, area integer GENERATED ALWAYS AS (w * 2) STORED);
