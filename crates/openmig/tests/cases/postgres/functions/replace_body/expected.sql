-- Let SQL function bodies reference objects created later
SET check_function_bodies = off;
-- Replace function "app"."answer"()
CREATE OR REPLACE FUNCTION "app"."answer"() RETURNS integer LANGUAGE sql VOLATILE CALLED ON NULL INPUT SECURITY INVOKER PARALLEL UNSAFE COST 100 AS $$SELECT 42$$;
