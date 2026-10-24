-- Add unique constraint "accounts_email_key" of "app"."accounts"
ALTER TABLE "app"."accounts" ADD CONSTRAINT "accounts_email_key" UNIQUE NULLS NOT DISTINCT ("email");
-- Drop check constraint "accounts_balance_positive" of "app"."accounts"
ALTER TABLE "app"."accounts" DROP CONSTRAINT "accounts_balance_positive";
-- Add check constraint "accounts_balance_positive" of "app"."accounts"
ALTER TABLE "app"."accounts" ADD CONSTRAINT "accounts_balance_positive" CHECK (balance >= 0::numeric);
