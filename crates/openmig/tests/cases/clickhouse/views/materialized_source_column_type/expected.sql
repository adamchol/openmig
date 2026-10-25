-- Modify column "x" of "app"."src"
ALTER TABLE `app`.`src` MODIFY COLUMN `x` UInt64;
-- Drop materialized view "app"."copy"
DROP VIEW IF EXISTS `app`.`copy` SYNC;
-- Create materialized view "app"."copy"
CREATE MATERIALIZED VIEW IF NOT EXISTS `app`.`copy` TO `app`.`dst` AS
SELECT x FROM app.src;
