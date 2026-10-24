-- Retyping an attribute of a composite type that a table column uses is unsupported.
-- unsupported
-- no_reverse
-- @both
CREATE SCHEMA app;
-- @a
CREATE TYPE app.addr AS (zip varchar(5));
CREATE TABLE app.t (home app.addr);
-- @b
CREATE TYPE app.addr AS (zip varchar(10));
CREATE TABLE app.t (home app.addr);
