-- Create extension "citext"
CREATE EXTENSION IF NOT EXISTS "citext" WITH SCHEMA "public";
-- Create schema "app"
CREATE SCHEMA "app";
-- Create table "app"."users"
CREATE TABLE "app"."users" ("email" public.citext);
-- Set comment on extension "citext"
COMMENT ON EXTENSION "citext" IS 'data type for case-insensitive character strings';
