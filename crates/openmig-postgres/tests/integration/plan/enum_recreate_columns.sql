-- With allow_recreate, an enum that a column uses is recreated by dropping and adding the column
-- again (destructive), with the view on it.
-- recreate
-- no_reverse
-- expect: DROP COLUMN "s"
-- @both
CREATE SCHEMA app;
-- @a
CREATE TYPE app.status AS ENUM ('a', 'b');
CREATE TABLE app.t (id integer, s app.status DEFAULT 'a');
CREATE VIEW app.v AS SELECT id, s FROM app.t;
-- @b
CREATE TYPE app.status AS ENUM ('b', 'a');
CREATE TABLE app.t (id integer, s app.status DEFAULT 'a');
CREATE VIEW app.v AS SELECT id, s FROM app.t;
