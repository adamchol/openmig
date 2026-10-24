-- Add value 'pending' to enum type "app"."status"
ALTER TYPE "app"."status" ADD VALUE 'pending' BEFORE 'active';
-- Add value 'suspended' to enum type "app"."status"
ALTER TYPE "app"."status" ADD VALUE 'suspended' AFTER 'active';
-- Alter column "status" of "app"."users"
ALTER TABLE "app"."users" ALTER COLUMN "status" SET DEFAULT 'pending'::app.status;
