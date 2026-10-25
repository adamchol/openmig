-- Create extension "btree_gist"
CREATE EXTENSION IF NOT EXISTS "btree_gist" WITH SCHEMA "public";
-- Create schema "app"
CREATE SCHEMA "app";
-- Create table "app"."bookings"
CREATE TABLE "app"."bookings" ("room" integer NOT NULL, "during" tstzrange NOT NULL, CONSTRAINT "bookings_no_overlap" EXCLUDE USING gist ("room" WITH =, "during" WITH &&));
-- Set comment on extension "btree_gist"
COMMENT ON EXTENSION "btree_gist" IS 'support for indexing common datatypes in GiST';
