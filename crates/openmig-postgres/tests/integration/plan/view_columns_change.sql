-- A view whose column types change is dropped and created again with its dependants, grants,
-- comments and triggers.
-- roles: omp_reader
-- expect: DROP VIEW "app"."outer"
-- @both
CREATE SCHEMA app;
CREATE TABLE app.t (id integer PRIMARY KEY, n integer);
CREATE FUNCTION app.ins() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$;
-- @a
CREATE VIEW app.inner_v AS SELECT id, n FROM app.t;
CREATE VIEW app.outer AS SELECT id FROM app.inner_v;
GRANT SELECT ON app.inner_v TO omp_reader;
COMMENT ON VIEW app.inner_v IS 'inner';
CREATE TRIGGER inner_ins INSTEAD OF INSERT ON app.inner_v FOR EACH ROW EXECUTE FUNCTION app.ins();
-- @b
CREATE VIEW app.inner_v AS SELECT id, n::bigint AS n FROM app.t;
CREATE VIEW app.outer AS SELECT id FROM app.inner_v;
GRANT SELECT ON app.inner_v TO omp_reader;
COMMENT ON VIEW app.inner_v IS 'inner';
CREATE TRIGGER inner_ins INSTEAD OF INSERT ON app.inner_v FOR EACH ROW EXECUTE FUNCTION app.ins();
