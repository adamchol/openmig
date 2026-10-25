-- Create database "analytics"
CREATE DATABASE IF NOT EXISTS `analytics` ENGINE = Atomic;
-- Create table "analytics"."types"
CREATE TABLE IF NOT EXISTS `analytics`.`types`
(
    `id` UUID,
    `flag` Bool,
    `status` Enum8('new' = 1, 'done' = 2),
    `tags` Array(String),
    `words` Array(Tuple(word String, count UInt32)),
    `price` Decimal(18, 4),
    `hash` FixedString(32),
    `maybe` Nullable(String),
    `alias_len` UInt64 ALIAS length(hash)
)
ENGINE = MergeTree
ORDER BY id
SETTINGS `index_granularity` = 8192;
