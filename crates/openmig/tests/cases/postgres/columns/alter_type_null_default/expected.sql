-- Alter column "label" of "app"."items"
ALTER TABLE "app"."items" ALTER COLUMN "label" DROP DEFAULT;
-- Alter column "label" of "app"."items"
ALTER TABLE "app"."items" ALTER COLUMN "label" TYPE text USING "label"::text;
-- Alter column "label" of "app"."items"
ALTER TABLE "app"."items" ALTER COLUMN "label" DROP NOT NULL;
-- Alter column "price" of "app"."items"
ALTER TABLE "app"."items" ALTER COLUMN "price" TYPE numeric(12,2) USING "price"::numeric(12,2);
-- Alter column "price" of "app"."items"
ALTER TABLE "app"."items" ALTER COLUMN "price" SET NOT NULL;
-- Alter column "price" of "app"."items"
ALTER TABLE "app"."items" ALTER COLUMN "price" SET DEFAULT 0;
