-- Rename table "app"."users" to "people"
ALTER TABLE "app"."users" RENAME TO "people";
-- Rename column "name" of table "app"."people" to "full_name"
ALTER TABLE "app"."people" RENAME COLUMN "name" TO "full_name";
