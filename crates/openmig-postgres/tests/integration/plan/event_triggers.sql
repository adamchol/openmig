-- Event triggers: create, drop, firing mode, recreate on tag changes.
-- expect: ALTER EVENT TRIGGER "omp_et_mode" ENABLE ALWAYS
-- @both
CREATE SCHEMA app;
CREATE FUNCTION app.on_ddl() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN END $$;
-- @a
CREATE EVENT TRIGGER omp_et_mode ON ddl_command_end EXECUTE FUNCTION app.on_ddl();
CREATE EVENT TRIGGER omp_et_tags ON ddl_command_end WHEN TAG IN ('CREATE TABLE') EXECUTE FUNCTION app.on_ddl();
CREATE EVENT TRIGGER omp_et_gone ON sql_drop EXECUTE FUNCTION app.on_ddl();
-- @b
CREATE EVENT TRIGGER omp_et_mode ON ddl_command_end EXECUTE FUNCTION app.on_ddl();
ALTER EVENT TRIGGER omp_et_mode ENABLE ALWAYS;
CREATE EVENT TRIGGER omp_et_tags ON ddl_command_end WHEN TAG IN ('CREATE TABLE', 'ALTER TABLE') EXECUTE FUNCTION app.on_ddl();
CREATE EVENT TRIGGER omp_et_new ON ddl_command_start EXECUTE FUNCTION app.on_ddl();
COMMENT ON EVENT TRIGGER omp_et_new IS 'new';
