-- Functions: CREATE OR REPLACE for body changes, ALTER for attributes, drop and create for
-- signature or result changes; grants and PUBLIC revokes; procedures.
-- roles: omp_runner
-- expect: CREATE OR REPLACE FUNCTION "app"."body"
-- expect: ALTER FUNCTION "app"."attrs"(integer)
-- expect: DROP FUNCTION "app"."ret"(integer)
-- expect: REVOKE EXECUTE ON FUNCTION "app"."secret"() FROM PUBLIC
-- @both
CREATE SCHEMA app;
-- @a
CREATE FUNCTION app.body(a integer) RETURNS integer LANGUAGE sql AS $$ SELECT a $$;
CREATE FUNCTION app.attrs(a integer) RETURNS integer LANGUAGE sql AS $$ SELECT a $$;
CREATE FUNCTION app.ret(a integer) RETURNS integer LANGUAGE sql AS $$ SELECT a $$;
CREATE FUNCTION app.argname(a integer) RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$;
CREATE FUNCTION app.secret() RETURNS void LANGUAGE plpgsql AS $$ BEGIN END $$;
CREATE FUNCTION app.overload(a integer) RETURNS integer LANGUAGE sql AS $$ SELECT a $$;
CREATE FUNCTION app.gone() RETURNS void LANGUAGE sql AS $$ $$;
CREATE PROCEDURE app.proc(IN a integer) LANGUAGE plpgsql AS $$ BEGIN END $$;
CREATE FUNCTION app.std(a integer) RETURNS integer LANGUAGE sql IMMUTABLE RETURN a + 1;
COMMENT ON FUNCTION app.body(integer) IS 'body';
-- @b
CREATE FUNCTION app.body(a integer) RETURNS integer LANGUAGE sql AS $$ SELECT a + 1 $$;
CREATE FUNCTION app.attrs(a integer) RETURNS integer LANGUAGE sql STABLE STRICT SECURITY DEFINER PARALLEL SAFE COST 5 SET search_path = pg_catalog, app AS $$ SELECT a $$;
CREATE FUNCTION app.ret(a integer) RETURNS bigint LANGUAGE sql AS $$ SELECT a::bigint $$;
CREATE FUNCTION app.argname(b integer) RETURNS integer LANGUAGE sql AS $$ SELECT 1 $$;
CREATE FUNCTION app.secret() RETURNS void LANGUAGE plpgsql AS $$ BEGIN END $$;
REVOKE EXECUTE ON FUNCTION app.secret() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION app.secret() TO omp_runner WITH GRANT OPTION;
CREATE FUNCTION app.overload(a integer) RETURNS integer LANGUAGE sql AS $$ SELECT a $$;
CREATE FUNCTION app.overload(a text) RETURNS text LANGUAGE sql AS $$ SELECT a $$;
CREATE PROCEDURE app.proc(IN a integer, INOUT b integer DEFAULT 0) LANGUAGE plpgsql AS $$ BEGIN b := a; END $$;
CREATE FUNCTION app.std(a integer) RETURNS integer LANGUAGE sql IMMUTABLE BEGIN ATOMIC SELECT a + 2; END;
CREATE FUNCTION app.setof(n integer) RETURNS TABLE (i integer) LANGUAGE sql ROWS 10 AS $$ SELECT generate_series(1, n) $$;
COMMENT ON FUNCTION app.body(integer) IS 'body!';
