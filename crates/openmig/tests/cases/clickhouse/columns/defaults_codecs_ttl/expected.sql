-- Modify column "a" of "app"."events"
ALTER TABLE `app`.`events` MODIFY COLUMN `a` MATERIALIZED concat('y', toString(ts));
-- Modify column "b" of "app"."events"
ALTER TABLE `app`.`events` MODIFY COLUMN `b` REMOVE CODEC;
-- Modify column "b" of "app"."events"
ALTER TABLE `app`.`events` MODIFY COLUMN `b` ALIAS toString(ts);
-- Modify column "c" of "app"."events"
ALTER TABLE `app`.`events` MODIFY COLUMN `c` REMOVE TTL;
-- Modify column "c" of "app"."events"
ALTER TABLE `app`.`events` MODIFY COLUMN `c` REMOVE COMMENT;
-- Modify column "d" of "app"."events"
ALTER TABLE `app`.`events` MODIFY COLUMN `d` DEFAULT toUInt32(1);
-- Modify column "d" of "app"."events"
ALTER TABLE `app`.`events` MODIFY COLUMN `d` CODEC(Delta(4), ZSTD(1));
-- Modify column "d" of "app"."events"
ALTER TABLE `app`.`events` MODIFY COLUMN `d` TTL ts + toIntervalDay(7);
