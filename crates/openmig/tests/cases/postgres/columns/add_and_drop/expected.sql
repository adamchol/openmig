-- Add column "created_at" to table "app"."users"
ALTER TABLE "app"."users" ADD COLUMN "created_at" timestamp with time zone DEFAULT now() NOT NULL;
-- Add column "name" to table "app"."users"
ALTER TABLE "app"."users" ADD COLUMN "name" character varying(200) DEFAULT 'anonymous'::character varying;
-- Drop column "legacy" of "app"."users"
ALTER TABLE "app"."users" DROP COLUMN "legacy";
