-- Drop view "app"."metric_values"
DROP VIEW "app"."metric_values";
-- Alter column "value" of "app"."metrics"
ALTER TABLE "app"."metrics" ALTER COLUMN "value" TYPE bigint USING "value"::bigint;
-- Create view "app"."metric_values"
CREATE VIEW "app"."metric_values" AS SELECT value
   FROM app.metrics;
