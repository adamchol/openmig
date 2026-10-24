-- Composite types: attributes added, dropped and retyped in place (retyping only while no
-- table column uses the type); reordering recreates a type that no table uses.
-- @both
CREATE SCHEMA app;
-- @a
CREATE TYPE app.addr AS (street text, zip varchar(5), city text);
CREATE TYPE app.loose AS (a integer, b text);
CREATE TYPE app.pair AS (a integer, b integer);
CREATE TABLE app.t (id integer, home app.addr);
CREATE FUNCTION app.swap(p app.pair) RETURNS app.pair LANGUAGE sql AS $$ SELECT ROW(p.b, p.a)::app.pair $$;
-- @b
CREATE TYPE app.addr AS (street text, zip varchar(5), country text COLLATE "C");
CREATE TYPE app.loose AS (a bigint, b text, c date);
CREATE TYPE app.pair AS (b integer, a integer);
CREATE TABLE app.t (id integer, home app.addr);
CREATE FUNCTION app.swap(p app.pair) RETURNS app.pair LANGUAGE sql AS $$ SELECT ROW(p.b, p.a)::app.pair $$;
