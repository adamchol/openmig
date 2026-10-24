-- Dropping a column that a view uses: the view changes first.
-- expect: CREATE OR REPLACE VIEW
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (id integer, gone text);
CREATE VIEW app.v AS SELECT id, gone FROM app.t;
-- @b
CREATE TABLE app.t (id integer);
CREATE VIEW app.v AS SELECT id, NULL::text AS gone FROM app.t;
