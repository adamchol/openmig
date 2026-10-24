-- Create schema "app"
CREATE SCHEMA "app";
-- Create table "app"."orgs"
CREATE TABLE "app"."orgs" ("id" integer NOT NULL, CONSTRAINT "orgs_pkey" PRIMARY KEY ("id"));
-- Create table "app"."members"
CREATE TABLE "app"."members" ("org_id" integer NOT NULL, "user_id" integer NOT NULL, CONSTRAINT "members_pkey" PRIMARY KEY ("org_id", "user_id"), CONSTRAINT "members_org_fk" FOREIGN KEY ("org_id") REFERENCES "app"."orgs" ("id") ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED);
