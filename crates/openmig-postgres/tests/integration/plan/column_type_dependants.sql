-- A column type change rebuilds the policy, trigger, generated column and SQL-standard function
-- that use the column; indexes and constraints are rebuilt by PostgreSQL itself.
-- reject: DROP INDEX
-- @both
CREATE SCHEMA app;
CREATE FUNCTION app.trg() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
-- @a
CREATE TABLE app.t (id integer PRIMARY KEY, n integer DEFAULT 1 CHECK (n > 0), g integer GENERATED ALWAYS AS (n * 2) STORED);
CREATE INDEX t_n_idx ON app.t (n);
CREATE TABLE app.r (n integer REFERENCES app.t (id));
ALTER TABLE app.t ENABLE ROW LEVEL SECURITY;
CREATE POLICY p ON app.t USING (n > 0);
CREATE TRIGGER t_trg BEFORE UPDATE OF n ON app.t FOR EACH ROW EXECUTE FUNCTION app.trg();
CREATE FUNCTION app.total() RETURNS bigint LANGUAGE sql STABLE BEGIN ATOMIC SELECT sum(n) FROM app.t; END;
-- @b
CREATE TABLE app.t (id integer PRIMARY KEY, n bigint DEFAULT 1 CHECK (n > 0), g bigint GENERATED ALWAYS AS (n * 2) STORED);
CREATE INDEX t_n_idx ON app.t (n);
CREATE TABLE app.r (n integer REFERENCES app.t (id));
ALTER TABLE app.t ENABLE ROW LEVEL SECURITY;
CREATE POLICY p ON app.t USING (n > 0);
CREATE TRIGGER t_trg BEFORE UPDATE OF n ON app.t FOR EACH ROW EXECUTE FUNCTION app.trg();
CREATE FUNCTION app.total() RETURNS numeric LANGUAGE sql STABLE BEGIN ATOMIC SELECT sum(n) FROM app.t; END;
