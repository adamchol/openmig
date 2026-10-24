-- Inheritance: children get only their own columns, parents are added and removed.
-- @both
CREATE SCHEMA app;
CREATE TABLE app.base (id integer, at timestamptz);
CREATE TABLE app.other (tag text);
-- @a
CREATE TABLE app.child (extra text) INHERITS (app.base);
CREATE TABLE app.mixed (tag text, x integer);
-- @b
CREATE TABLE app.child (extra text, more integer) INHERITS (app.base);
CREATE TABLE app.mixed (tag text, x integer) INHERITS (app.other);
CREATE TABLE app.fresh (y integer) INHERITS (app.base, app.other);
