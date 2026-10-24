-- A function that must be dropped and created again (its result type changes) takes its
-- dependants with it: a column default, a check, an expression index, a policy, a view, a
-- trigger and a SQL-standard function.
-- expect: DROP FUNCTION "app"."f"(integer)
-- @both
CREATE SCHEMA app;
-- @a
CREATE FUNCTION app.f(x integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $$ SELECT x + 1 $$;
CREATE FUNCTION app.trg() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
CREATE TABLE app.t (
  id integer PRIMARY KEY,
  v integer DEFAULT app.f(1),
  CONSTRAINT t_v_check CHECK (app.f(v) > 0)
);
CREATE INDEX t_f_idx ON app.t (app.f(id));
ALTER TABLE app.t ENABLE ROW LEVEL SECURITY;
CREATE POLICY p ON app.t USING (app.f(id) > 0);
CREATE VIEW app.v AS SELECT id, app.f(v) AS fv FROM app.t;
CREATE TRIGGER t_trg BEFORE UPDATE ON app.t FOR EACH ROW WHEN (app.f(new.id) > 0) EXECUTE FUNCTION app.trg();
CREATE FUNCTION app.g(x integer) RETURNS integer LANGUAGE sql IMMUTABLE RETURN app.f(x) * 2;
-- @b
CREATE FUNCTION app.f(x integer) RETURNS bigint LANGUAGE sql IMMUTABLE AS $$ SELECT x + 1 $$;
CREATE FUNCTION app.trg() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
CREATE TABLE app.t (
  id integer PRIMARY KEY,
  v integer DEFAULT app.f(1),
  CONSTRAINT t_v_check CHECK (app.f(v) > 0)
);
CREATE INDEX t_f_idx ON app.t (app.f(id));
ALTER TABLE app.t ENABLE ROW LEVEL SECURITY;
CREATE POLICY p ON app.t USING (app.f(id) > 0);
CREATE VIEW app.v AS SELECT id, app.f(v) AS fv FROM app.t;
CREATE TRIGGER t_trg BEFORE UPDATE ON app.t FOR EACH ROW WHEN (app.f(new.id) > 0) EXECUTE FUNCTION app.trg();
CREATE FUNCTION app.g(x integer) RETURNS bigint LANGUAGE sql IMMUTABLE RETURN app.f(x) * 2;
