-- Drop index "call_id_minmax" from "analytics"."events"
ALTER TABLE `analytics`.`events` DROP INDEX IF EXISTS `call_id_minmax`;
-- Add index "call_id_set" to "analytics"."events"
ALTER TABLE `analytics`.`events` ADD INDEX IF NOT EXISTS `call_id_set` call_id TYPE set(1024) GRANULARITY 1;
