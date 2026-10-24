-- Grant on schema "app" to "app_reader"
GRANT USAGE ON SCHEMA "app" TO "app_reader";
-- Grant on schema "app" to "app_writer"
GRANT USAGE ON SCHEMA "app" TO "app_writer";
-- Revoke on table "app"."orgs" from "app_writer"
REVOKE SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER, MAINTAIN ON TABLE "app"."orgs" FROM "app_writer";
-- Grant on table "app"."orgs" to "app_reader"
GRANT SELECT ON TABLE "app"."orgs" TO "app_reader";
-- Grant on table "app"."users" to "app_reader"
GRANT SELECT ON TABLE "app"."users" TO "app_reader";
-- Grant on column "email" of "app"."users" to "app_writer"
GRANT UPDATE ("email") ON TABLE "app"."users" TO "app_writer" WITH GRANT OPTION;
