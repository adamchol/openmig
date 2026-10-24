-- Create schema "app"
CREATE SCHEMA "app";
-- Create function "app"."on_ddl"()
CREATE FUNCTION "app"."on_ddl"() RETURNS event_trigger LANGUAGE plpgsql VOLATILE CALLED ON NULL INPUT SECURITY INVOKER PARALLEL UNSAFE COST 100 AS $$BEGIN RAISE NOTICE 'ddl'; END;$$;
-- Create event trigger "notify_ddl"
CREATE EVENT TRIGGER "notify_ddl" ON ddl_command_end WHEN TAG IN ('ALTER TABLE', 'CREATE TABLE') EXECUTE FUNCTION "app"."on_ddl"();
