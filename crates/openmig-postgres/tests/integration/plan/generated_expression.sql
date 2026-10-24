-- PostgreSQL 17 changes a generated expression in place, even under a view.
-- min_major: 17
-- expect: SET EXPRESSION AS
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (w integer, h integer, area integer GENERATED ALWAYS AS (w * h) STORED);
CREATE VIEW app.v AS SELECT area FROM app.t;
-- @b
CREATE TABLE app.t (w integer, h integer, area integer GENERATED ALWAYS AS (w * h * 2) STORED);
CREATE VIEW app.v AS SELECT area FROM app.t;
