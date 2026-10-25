-- Drop projection "by_kind" from "app"."events"
ALTER TABLE `app`.`events` DROP PROJECTION IF EXISTS `by_kind`;
-- Drop constraint "id_positive" from "app"."events"
ALTER TABLE `app`.`events` DROP CONSTRAINT IF EXISTS `id_positive`;
-- Add projection "sorted" to "app"."events"
ALTER TABLE `app`.`events` ADD PROJECTION IF NOT EXISTS `sorted` (SELECT * ORDER BY kind);
-- Add constraint "id_positive" to "app"."events"
ALTER TABLE `app`.`events` ADD CONSTRAINT IF NOT EXISTS `id_positive` CHECK id > 1;
-- Add constraint "kind_known" to "app"."events"
ALTER TABLE `app`.`events` ADD CONSTRAINT IF NOT EXISTS `kind_known` ASSUME kind != '';
-- Modify SAMPLE BY of "app"."events"
ALTER TABLE `app`.`events` MODIFY SAMPLE BY id;
-- Reset settings of "app"."events"
ALTER TABLE `app`.`events` RESET SETTING `merge_with_ttl_timeout`;
-- Comment on table "app"."events"
ALTER TABLE `app`.`events` MODIFY COMMENT 'all events';
