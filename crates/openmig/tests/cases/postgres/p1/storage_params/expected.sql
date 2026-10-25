-- Alter column "v" of "app"."hot"
ALTER TABLE "app"."hot" ALTER COLUMN "v" SET STORAGE EXTERNAL;
-- Set persistence of table "app"."hot"
ALTER TABLE "app"."hot" SET UNLOGGED;
-- Set storage parameters of table "app"."hot"
ALTER TABLE "app"."hot" SET (autovacuum_enabled = false, fillfactor = 70);
