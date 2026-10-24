-- Let SQL function bodies reference objects created later
SET check_function_bodies = off;
-- Create schema "app"
CREATE SCHEMA "app";
-- Create function "app"."fmt"(boolean)
CREATE FUNCTION "app"."fmt"("v" boolean) RETURNS text LANGUAGE sql VOLATILE CALLED ON NULL INPUT SECURITY INVOKER PARALLEL UNSAFE COST 100 AS $$SELECT CASE WHEN v THEN 'yes' ELSE 'no' END$$;
-- Create function "app"."fmt"(integer)
CREATE FUNCTION "app"."fmt"("v" integer) RETURNS text LANGUAGE sql VOLATILE CALLED ON NULL INPUT SECURITY INVOKER PARALLEL UNSAFE COST 100 AS $$SELECT v::text$$;
