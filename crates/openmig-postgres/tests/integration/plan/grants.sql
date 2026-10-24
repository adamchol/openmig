-- Privileges on schemas, tables, columns, sequences, types, functions and procedures for
-- declared roles and PUBLIC, including grant options; unmanaged roles are left alone.
-- roles: omp_web, omp_admin
-- expect: REVOKE GRANT OPTION FOR
-- expect: REVOKE USAGE ON TYPE "app"."status" FROM PUBLIC
-- reject: omp_unmanaged
-- @both
CREATE SCHEMA app;
CREATE TYPE app.status AS ENUM ('a');
CREATE SEQUENCE app.s;
CREATE TABLE app.t (id integer, secret text);
CREATE FUNCTION app.f() RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$;
CREATE PROCEDURE app.p() LANGUAGE sql AS $$ SELECT 1 $$;
DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'omp_unmanaged') THEN CREATE ROLE omp_unmanaged; END IF; END $$;
GRANT SELECT ON app.t TO omp_unmanaged;
-- @a
GRANT USAGE ON SCHEMA app TO omp_web;
GRANT SELECT, INSERT ON app.t TO omp_web WITH GRANT OPTION;
GRANT UPDATE (secret) ON app.t TO omp_admin;
GRANT USAGE ON SEQUENCE app.s TO omp_web;
REVOKE EXECUTE ON FUNCTION app.f() FROM PUBLIC;
-- @b
GRANT USAGE, CREATE ON SCHEMA app TO omp_web;
GRANT SELECT, INSERT, DELETE ON app.t TO omp_web;
GRANT SELECT (id), UPDATE (id) ON app.t TO omp_admin;
GRANT USAGE, SELECT ON SEQUENCE app.s TO omp_admin;
REVOKE USAGE ON TYPE app.status FROM PUBLIC;
GRANT USAGE ON TYPE app.status TO omp_web;
GRANT EXECUTE ON FUNCTION app.f() TO omp_admin;
REVOKE EXECUTE ON PROCEDURE app.p() FROM PUBLIC;
GRANT ALL ON app.t TO omp_admin;
