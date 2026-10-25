-- Create database "app"
CREATE DATABASE IF NOT EXISTS `app` ENGINE = Atomic;
-- Create table "app"."daily"
CREATE TABLE IF NOT EXISTS `app`.`daily`
(
    `day` Date,
    `n` UInt64
)
ENGINE = MergeTree
ORDER BY day
SETTINGS `index_granularity` = 8192;
-- Create table "app"."events"
CREATE TABLE IF NOT EXISTS `app`.`events`
(
    `day` Date,
    `kind` String
)
ENGINE = MergeTree
ORDER BY day
SETTINGS `index_granularity` = 8192;
-- Create materialized view "app"."daily_refresh"
CREATE MATERIALIZED VIEW IF NOT EXISTS `app`.`daily_refresh` REFRESH EVERY 1 HOUR TO `app`.`daily` SQL SECURITY DEFINER AS
SELECT day, count() AS n FROM app.events GROUP BY day;
-- Create materialized view "app"."kind_counts"
CREATE MATERIALIZED VIEW IF NOT EXISTS `app`.`kind_counts`
ENGINE = SummingMergeTree
PARTITION BY tuple()
ORDER BY kind
SETTINGS `index_granularity` = 8192 AS
SELECT kind, count() AS n FROM app.events GROUP BY kind;
