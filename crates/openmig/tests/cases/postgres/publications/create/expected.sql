-- Create schema "app"
CREATE SCHEMA "app";
-- Create schema "events"
CREATE SCHEMA "events";
-- Create table "app"."grants"
CREATE TABLE "app"."grants" ("subject" text NOT NULL, "session_id" uuid NOT NULL, "secret" text, CONSTRAINT "grants_pkey" PRIMARY KEY ("subject", "session_id"));
-- Create table "app"."sessions"
CREATE TABLE "app"."sessions" ("id" uuid NOT NULL, CONSTRAINT "sessions_pkey" PRIMARY KEY ("id"));
-- Create publication "app_cdc"
CREATE PUBLICATION "app_cdc" FOR TABLE "app"."grants" ("subject", "session_id") WHERE ((subject <> 'system'::text)), TABLE "app"."sessions" WITH (publish = 'insert, update, delete');
-- Create publication "app_empty"
CREATE PUBLICATION "app_empty";
-- Create publication "app_events"
CREATE PUBLICATION "app_events" FOR TABLES IN SCHEMA "events" WITH (publish_via_partition_root = true);
-- Set comment on publication "app_cdc"
COMMENT ON PUBLICATION "app_cdc" IS 'read by the CDC reader';
