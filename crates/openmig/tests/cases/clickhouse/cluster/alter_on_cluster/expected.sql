-- Create role "replicated_reader"
CREATE ROLE IF NOT EXISTS `replicated_reader` IN `replicated`;
-- Comment on database "repl"
ALTER DATABASE `repl` ON CLUSTER `{cluster}` MODIFY COMMENT 'replicated';
-- Create function "double"
CREATE OR REPLACE FUNCTION `double` ON CLUSTER `{cluster}` AS (`x`) -> x * 2;
-- Add column "kind" to "plain"."events"
ALTER TABLE `plain`.`events` ON CLUSTER `{cluster}` ADD COLUMN IF NOT EXISTS `kind` String AFTER `id`;
-- Add column "kind" to "repl"."events"
ALTER TABLE `repl`.`events` ADD COLUMN IF NOT EXISTS `kind` LowCardinality(String) AFTER `id`;
-- Grant on "plain".* to "local_reader"
GRANT ON CLUSTER `{cluster}` SELECT ON `plain`.* TO `local_reader`;
-- Grant on "repl".* to "local_reader"
GRANT ON CLUSTER `{cluster}` SELECT ON `repl`.* TO `local_reader`;
-- Grant on "plain".* to "replicated_reader"
GRANT SELECT ON `plain`.* TO `replicated_reader`;
-- Grant on "repl".* to "replicated_reader"
GRANT SELECT ON `repl`.* TO `replicated_reader`;
