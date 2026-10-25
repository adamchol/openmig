-- Modify column "kind" of "analytics"."events"
ALTER TABLE `analytics`.`events` MODIFY COLUMN `kind` LowCardinality(String);
-- Modify column "kind" of "analytics"."events"
ALTER TABLE `analytics`.`events` COMMENT COLUMN `kind` 'Event kind';
-- Modify column "payload" of "analytics"."events"
ALTER TABLE `analytics`.`events` MODIFY COLUMN `payload` CODEC(ZSTD(3));
-- Add column "user_id" to "analytics"."events"
ALTER TABLE `analytics`.`events` ADD COLUMN IF NOT EXISTS `user_id` UInt64 DEFAULT 0 AFTER `kind`;
-- Drop column "legacy" from "analytics"."events"
ALTER TABLE `analytics`.`events` DROP COLUMN IF EXISTS `legacy`;
