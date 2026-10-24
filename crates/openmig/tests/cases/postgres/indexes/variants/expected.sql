-- Create schema "app"
CREATE SCHEMA "app";
-- Create table "app"."users"
CREATE TABLE "app"."users" ("id" integer NOT NULL, "email" text NOT NULL, "tenant_id" integer, "created_at" timestamp with time zone, "profile" jsonb);
-- Create index "users_id_hash_idx" on "app"."users"
CREATE INDEX "users_id_hash_idx" ON "app"."users" USING hash ("id");
-- Create index "users_lower_email_key" on "app"."users"
CREATE UNIQUE INDEX "users_lower_email_key" ON "app"."users" USING btree ((lower(email)));
-- Create index "users_profile_idx" on "app"."users"
CREATE INDEX "users_profile_idx" ON "app"."users" USING gin ("profile" jsonb_path_ops);
-- Create index "users_recent_idx" on "app"."users"
CREATE INDEX "users_recent_idx" ON "app"."users" USING btree ("tenant_id", "created_at" DESC NULLS LAST) WHERE (created_at IS NOT NULL);
-- Create index "users_tenant_cover_idx" on "app"."users"
CREATE INDEX "users_tenant_cover_idx" ON "app"."users" USING btree ("tenant_id") INCLUDE ("email");
