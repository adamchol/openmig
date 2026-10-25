-- Rename table "app"."hits" to "app"."visits"
RENAME TABLE `app`.`hits` TO `app`.`visits`;
-- Rename column "url" to "page" in "app"."visits"
ALTER TABLE `app`.`visits` RENAME COLUMN IF EXISTS `url` TO `page`;
