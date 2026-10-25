-- Add columns to the sorting key of "app"."events"
ALTER TABLE `app`.`events`
    ADD COLUMN IF NOT EXISTS `c` UInt8 AFTER `b`,
    MODIFY ORDER BY (a, c);
