-- Create schema "app"
CREATE SCHEMA "app";
-- Create table "app"."invoices"
CREATE TABLE "app"."invoices" ("id" integer, "order_id" integer);
-- Create table "app"."orders"
CREATE TABLE "app"."orders" ("id" integer);
-- Set comment on schema "app"
COMMENT ON SCHEMA "app" IS 'Tenant acme';
-- Set comment on table "app"."invoices"
COMMENT ON TABLE "app"."invoices" IS 'ACME invoices';
