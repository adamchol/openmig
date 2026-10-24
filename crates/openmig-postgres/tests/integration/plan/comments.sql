-- Comments on every kind of object: set, change and remove.
-- @both
CREATE SCHEMA app;
CREATE EXTENSION IF NOT EXISTS citext WITH SCHEMA public;
CREATE TYPE app.e AS ENUM ('a');
CREATE TYPE app.c AS (x integer);
CREATE DOMAIN app.d AS integer CONSTRAINT d_check CHECK (VALUE > 0);
CREATE SEQUENCE app.s;
CREATE TABLE app.t (id integer CONSTRAINT t_pkey PRIMARY KEY, n integer CONSTRAINT t_n_check CHECK (n > 0));
CREATE INDEX t_n_idx ON app.t (n);
CREATE VIEW app.v AS SELECT id FROM app.t;
CREATE MATERIALIZED VIEW app.m AS SELECT id FROM app.t;
CREATE FUNCTION app.f() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
CREATE PROCEDURE app.p() LANGUAGE sql AS $$ SELECT 1 $$;
CREATE TRIGGER tr BEFORE INSERT ON app.t FOR EACH ROW EXECUTE FUNCTION app.f();
CREATE POLICY po ON app.t USING (true);
-- @a
COMMENT ON SCHEMA app IS 'a';
COMMENT ON TYPE app.e IS 'a';
COMMENT ON TABLE app.t IS 'a';
COMMENT ON COLUMN app.t.n IS 'a';
COMMENT ON INDEX app.t_n_idx IS 'a';
COMMENT ON FUNCTION app.f() IS 'a';
COMMENT ON EXTENSION citext IS 'a';
-- @b
COMMENT ON SCHEMA app IS 'b';
COMMENT ON TYPE app.c IS 'b';
COMMENT ON DOMAIN app.d IS 'b';
COMMENT ON CONSTRAINT d_check ON DOMAIN app.d IS 'b';
COMMENT ON SEQUENCE app.s IS 'b';
COMMENT ON TABLE app.t IS 'it''s b';
COMMENT ON CONSTRAINT t_pkey ON app.t IS 'b';
COMMENT ON CONSTRAINT t_n_check ON app.t IS 'b';
COMMENT ON VIEW app.v IS 'b';
COMMENT ON COLUMN app.v.id IS 'b';
COMMENT ON MATERIALIZED VIEW app.m IS 'b';
COMMENT ON COLUMN app.m.id IS 'b';
COMMENT ON PROCEDURE app.p() IS 'b';
COMMENT ON TRIGGER tr ON app.t IS 'b';
COMMENT ON POLICY po ON app.t IS 'b';
