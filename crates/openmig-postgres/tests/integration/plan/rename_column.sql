-- A column renamed through the rename set keeps its data and is changed in the same plan; the
-- view on it keeps working.
-- rename: column app.t.name app.t.full_name
-- expect: RENAME COLUMN "name" TO "full_name"
-- reject: DROP COLUMN
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (id integer PRIMARY KEY, name varchar(50) NOT NULL CHECK (name <> ''));
CREATE INDEX t_name_idx ON app.t (name);
CREATE VIEW app.v AS SELECT id, name FROM app.t;
-- @b
CREATE TABLE app.t (id integer PRIMARY KEY, full_name text NOT NULL CHECK (full_name <> ''));
CREATE INDEX t_name_idx ON app.t (full_name);
CREATE VIEW app.v AS SELECT id, full_name AS name FROM app.t;
