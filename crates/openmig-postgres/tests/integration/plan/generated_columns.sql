-- A stored generated column can become a plain column (DROP EXPRESSION); new generated columns
-- are added; the reverse direction needs SET EXPRESSION or is unsupported, so only forward.
-- no_reverse
-- expect: DROP EXPRESSION
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (w integer, h integer, area integer GENERATED ALWAYS AS (w * h) STORED);
-- @b
CREATE TABLE app.t (w integer, h integer, area integer, perimeter integer GENERATED ALWAYS AS (2 * (w + h)) STORED);
