-- Domains: default, NOT NULL and checks change in place; a domain nobody uses is recreated when
-- its base type changes.
-- expect: ALTER DOMAIN "app"."d" ADD CONSTRAINT "d_max" CHECK
-- @both
CREATE SCHEMA app;
-- @a
CREATE DOMAIN app.d AS integer DEFAULT 1 CONSTRAINT d_pos CHECK (VALUE > 0) CONSTRAINT d_gone CHECK (VALUE <> 7);
CREATE DOMAIN app.retyped AS integer;
CREATE TABLE app.t (v app.d);
ALTER DOMAIN app.d ADD CONSTRAINT d_valid CHECK (VALUE < 1000) NOT VALID;
-- @b
CREATE DOMAIN app.d AS integer NOT NULL CONSTRAINT d_pos CHECK (VALUE >= 0) CONSTRAINT d_max CHECK (VALUE < 100) CONSTRAINT d_valid CHECK (VALUE < 1000);
CREATE DOMAIN app.retyped AS bigint DEFAULT 0;
CREATE TABLE app.t (v app.d);
