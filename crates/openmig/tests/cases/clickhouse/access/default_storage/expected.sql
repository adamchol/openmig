-- Create role "admin"
CREATE ROLE IF NOT EXISTS `admin`;
-- Create user "loader"
CREATE USER IF NOT EXISTS `loader` IDENTIFIED WITH no_password;
-- Create role "reader"
CREATE ROLE IF NOT EXISTS `reader`;
-- Create database "app"
CREATE DATABASE IF NOT EXISTS `app` ENGINE = Atomic;
-- Create table "app"."events"
CREATE TABLE IF NOT EXISTS `app`.`events`
(
    `tenant` String
)
ENGINE = MergeTree
ORDER BY tenant
SETTINGS `index_granularity` = 8192;
-- Create row policy "tenant_a" on "app"."events"
CREATE ROW POLICY OR REPLACE `tenant_a` ON `app`.`events` FOR SELECT USING tenant = 'a' AS PERMISSIVE TO `reader`;
-- Grant role "reader" to "admin"
GRANT `reader` TO `admin`;
-- Grant role "reader" to "loader"
GRANT `reader` TO `loader`;
