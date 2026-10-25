-- Modify TTL of "analytics"."events"
ALTER TABLE `analytics`.`events` MODIFY TTL timestamp + toIntervalDay(90);
-- Modify settings of "analytics"."events"
ALTER TABLE `analytics`.`events` MODIFY SETTING `ttl_only_drop_parts` = 1;
