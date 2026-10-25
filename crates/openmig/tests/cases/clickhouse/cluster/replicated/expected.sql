-- Create database "dialo"
CREATE DATABASE IF NOT EXISTS `dialo` ON CLUSTER `{cluster}` ENGINE = Replicated('/clickhouse/databases/dialo', '{shard}', '{replica}');
-- Create database "plain"
CREATE DATABASE IF NOT EXISTS `plain` ON CLUSTER `{cluster}` ENGINE = Atomic;
-- Create table "dialo"."events"
CREATE TABLE IF NOT EXISTS `dialo`.`events`
(
    `call_id` String,
    `timestamp` DateTime64(9)
)
ENGINE = ReplicatedMergeTree('/clickhouse/tables/{uuid}/{shard}', '{replica}')
ORDER BY timestamp
SETTINGS `index_granularity` = 8192;
-- Create table "plain"."events"
CREATE TABLE IF NOT EXISTS `plain`.`events` ON CLUSTER `{cluster}`
(
    `timestamp` DateTime64(9)
)
ENGINE = MergeTree
ORDER BY timestamp
SETTINGS `index_granularity` = 8192;
