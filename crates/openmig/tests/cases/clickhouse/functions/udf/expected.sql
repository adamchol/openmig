-- Create database "app"
CREATE DATABASE IF NOT EXISTS `app` ENGINE = Atomic;
-- Replace function "add_one"
CREATE OR REPLACE FUNCTION `add_one` AS (`x`) -> x + 2;
-- Create function "linear"
CREATE OR REPLACE FUNCTION `linear` AS (`x`, `k`, `b`) -> (k * x) + b;
-- Create table "app"."t"
CREATE TABLE IF NOT EXISTS `app`.`t`
(
    `x` UInt64,
    `y` UInt64 DEFAULT (2 * x) + 1
)
ENGINE = MergeTree
ORDER BY x
SETTINGS `index_granularity` = 8192;
-- Drop function "legacy"
DROP FUNCTION IF EXISTS `legacy`;
