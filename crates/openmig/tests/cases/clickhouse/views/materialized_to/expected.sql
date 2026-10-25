-- Create database "otel"
CREATE DATABASE IF NOT EXISTS `otel` ENGINE = Atomic;
-- Create table "otel"."trace_ranges"
CREATE TABLE IF NOT EXISTS `otel`.`trace_ranges`
(
    `TraceId` String,
    `Start` DateTime64(9),
    `End` DateTime64(9)
)
ENGINE = ReplacingMergeTree
ORDER BY TraceId
SETTINGS `index_granularity` = 8192;
-- Create table "otel"."traces"
CREATE TABLE IF NOT EXISTS `otel`.`traces`
(
    `TraceId` String,
    `Timestamp` DateTime64(9)
)
ENGINE = MergeTree
ORDER BY (TraceId, Timestamp)
SETTINGS `index_granularity` = 8192;
-- Create materialized view "otel"."trace_ranges_mv"
CREATE MATERIALIZED VIEW IF NOT EXISTS `otel`.`trace_ranges_mv` TO `otel`.`trace_ranges` AS
SELECT TraceId, min(Timestamp) AS Start, max(Timestamp) AS End FROM otel.traces WHERE TraceId != '' GROUP BY TraceId;
