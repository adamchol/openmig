-- Create database "analytics"
CREATE DATABASE IF NOT EXISTS `analytics` ENGINE = Atomic COMMENT 'Event store';
-- Create table "analytics"."logs"
CREATE TABLE IF NOT EXISTS `analytics`.`logs`
(
    `service` LowCardinality(String) CODEC(ZSTD(1)),
    `ts` DateTime64(9) CODEC(Delta(8), ZSTD(1)),
    `day` Date MATERIALIZED toDate(ts),
    `body` String DEFAULT '' COMMENT 'Raw log line',
    `attrs` Map(LowCardinality(String), String),
    `updated_at` DateTime,
    INDEX `attr_keys` mapKeys(attrs) TYPE bloom_filter(0.01) GRANULARITY 1,
    INDEX `body_tokens` body TYPE tokenbf_v1(32768, 3, 0) GRANULARITY 4
)
ENGINE = ReplacingMergeTree(updated_at)
PARTITION BY toYYYYMM(day)
PRIMARY KEY (service, day)
ORDER BY (service, day, ts)
TTL day + toIntervalDay(30)
SETTINGS `index_granularity` = 4096, `ttl_only_drop_parts` = 1
COMMENT 'Application logs';
