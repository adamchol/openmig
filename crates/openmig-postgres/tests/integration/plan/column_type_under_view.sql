-- A column type change under a view (and a view on that view) rebuilds the views.
-- expect: DROP VIEW "app"."v2"
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (id integer PRIMARY KEY, amount integer);
CREATE VIEW app.v AS SELECT id, amount FROM app.t;
CREATE VIEW app.v2 AS SELECT id, amount * 2 AS double FROM app.v;
-- @b
CREATE TABLE app.t (id integer PRIMARY KEY, amount bigint);
CREATE VIEW app.v AS SELECT id, amount FROM app.t;
CREATE VIEW app.v2 AS SELECT id, amount * 2 AS double FROM app.v;
