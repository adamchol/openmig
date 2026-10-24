-- Create schema "app"
CREATE SCHEMA "app";
-- Create table "app"."a"
CREATE TABLE "app"."a" ("id" integer NOT NULL, "b_id" integer, CONSTRAINT "a_pkey" PRIMARY KEY ("id"));
-- Create table "app"."b"
CREATE TABLE "app"."b" ("id" integer NOT NULL, "a_id" integer, CONSTRAINT "b_pkey" PRIMARY KEY ("id"));
-- Add foreign key "a_b_fk" of "app"."a"
ALTER TABLE "app"."a" ADD CONSTRAINT "a_b_fk" FOREIGN KEY ("b_id") REFERENCES "app"."b" ("id");
-- Add foreign key "b_a_fk" of "app"."b"
ALTER TABLE "app"."b" ADD CONSTRAINT "b_a_fk" FOREIGN KEY ("a_id") REFERENCES "app"."a" ("id") ON DELETE SET NULL;
