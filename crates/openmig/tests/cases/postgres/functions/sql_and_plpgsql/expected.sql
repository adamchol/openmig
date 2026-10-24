-- Let SQL function bodies reference objects created later
SET check_function_bodies = off;
-- Create schema "app"
CREATE SCHEMA "app";
-- Create function "app"."add"(integer, integer)
CREATE FUNCTION "app"."add"("a" integer, "b" integer DEFAULT 1) RETURNS integer LANGUAGE sql IMMUTABLE STRICT SECURITY INVOKER PARALLEL UNSAFE COST 100 AS $$SELECT a + b$$;
-- Create function "app"."current_tenant"()
CREATE FUNCTION "app"."current_tenant"() RETURNS integer LANGUAGE plpgsql STABLE CALLED ON NULL INPUT SECURITY DEFINER PARALLEL UNSAFE COST 100 SET search_path TO 'pg_catalog', 'public' AS $$BEGIN
  RETURN nullif(current_setting('app.tenant', true), '')::integer;
END;
$$;
-- Create procedure "app"."noop"(text)
CREATE PROCEDURE "app"."noop"("msg" text) LANGUAGE plpgsql SECURITY INVOKER AS $$BEGIN RAISE NOTICE '%', msg; END;$$;
