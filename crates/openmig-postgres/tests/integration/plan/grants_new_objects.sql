-- New objects start with the default privileges; the plan grants and revokes from there.
-- roles: omp_web
-- expect: REVOKE EXECUTE ON FUNCTION "app"."f"() FROM PUBLIC
-- @both
CREATE SCHEMA app;
-- @a
-- @b
CREATE TABLE app.t (id integer);
GRANT SELECT ON app.t TO omp_web;
GRANT SELECT (id) ON app.t TO PUBLIC;
CREATE FUNCTION app.f() RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$;
REVOKE ALL ON FUNCTION app.f() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app.f() TO omp_web;
CREATE DOMAIN app.d AS integer;
REVOKE USAGE ON DOMAIN app.d FROM PUBLIC;
CREATE VIEW app.v AS SELECT id FROM app.t;
GRANT SELECT ON app.v TO omp_web;
CREATE SCHEMA other;
GRANT USAGE ON SCHEMA other TO omp_web;
