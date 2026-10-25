-- Create database "otel"
CREATE DATABASE IF NOT EXISTS `otel` ENGINE = Atomic;
-- Create table "otel"."metrics"
CREATE TABLE IF NOT EXISTS `otel`.`metrics`
ENGINE = TimeSeries
DATA ENGINE = MergeTree ORDER BY (id, timestamp)
TAGS ENGINE = AggregatingMergeTree PRIMARY KEY metric_name ORDER BY tuple(metric_name, id)
METRICS ENGINE = ReplacingMergeTree ORDER BY metric_family_name;
