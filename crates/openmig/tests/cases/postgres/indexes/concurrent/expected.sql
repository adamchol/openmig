-- Create index "users_email_idx" on "app"."users"
DROP INDEX CONCURRENTLY IF EXISTS "app"."users_email_idx";
-- Create index "users_email_idx" on "app"."users"
CREATE INDEX CONCURRENTLY "users_email_idx" ON "app"."users" USING btree ("email");
-- Drop index "users_old_idx" of "app"."users"
DROP INDEX CONCURRENTLY "app"."users_old_idx";
