-- Create schema "app"
CREATE SCHEMA "app";
-- Create table "app"."docs"
CREATE TABLE "app"."docs" ("tags" text[] DEFAULT '{}'::text[] NOT NULL, "matrix" integer[], "title" text COLLATE "C", "doc" jsonb);
