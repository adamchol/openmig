-- Create schema "archive"
CREATE SCHEMA "archive";
-- Move table "app"."logs" to schema "archive"
ALTER TABLE "app"."logs" SET SCHEMA "archive";
