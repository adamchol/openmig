-- Schemas with contents are created and dropped (contents first).
-- @both
CREATE SCHEMA keep;
-- @a
CREATE SCHEMA old;
CREATE TYPE old.e AS ENUM ('x');
CREATE TABLE old.t (id integer PRIMARY KEY, e old.e);
CREATE VIEW old.v AS SELECT id FROM old.t;
CREATE FUNCTION old.f() RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$;
COMMENT ON SCHEMA keep IS 'kept';
-- @b
CREATE SCHEMA fresh;
CREATE TABLE fresh.t (id integer PRIMARY KEY, k integer REFERENCES fresh.t (id));
CREATE SEQUENCE fresh.s;
COMMENT ON SCHEMA fresh IS 'new';
