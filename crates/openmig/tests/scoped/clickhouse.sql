CREATE TABLE default.their_events (id UInt64, ts DateTime) ENGINE = MergeTree ORDER BY id;
CREATE DATABASE analytics;
CREATE TABLE analytics.x (a String) ENGINE = MergeTree ORDER BY a;
CREATE USER their_user IDENTIFIED WITH plaintext_password BY 'x';
GRANT SELECT ON default.* TO their_user;
CREATE ROLE their_role;
CREATE ROW POLICY their_pol ON default.their_events USING id > 0 TO their_role;
CREATE FUNCTION their_fn AS (x) -> x + 1;
