-- Delete a row from "app"."statuses"
DELETE FROM "app"."statuses" WHERE "id" = '3';
-- Update a row of "app"."statuses"
UPDATE "app"."statuses" SET "name" = 'disabled' WHERE "id" = '2';
-- Insert rows into "app"."statuses"
INSERT INTO "app"."statuses" ("id", "name") VALUES ('4', 'archived');
