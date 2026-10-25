-- Create database "analytics"
CREATE DATABASE IF NOT EXISTS `analytics` ENGINE = Atomic;
-- Create table "analytics"."events"
CREATE TABLE IF NOT EXISTS `analytics`.`events`
(
    `timestamp` DateTime64(3),
    `kind` LowCardinality(String),
    `payload` String
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(timestamp)
ORDER BY timestamp
SETTINGS `index_granularity` = 8192;
