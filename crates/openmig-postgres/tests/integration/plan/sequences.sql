-- Sequences: create, drop, options, OWNED BY; serial columns; tables owning sequences.
-- @both
CREATE SCHEMA app;
-- @a
CREATE SEQUENCE app.s1;
CREATE SEQUENCE app.s2 AS integer START 5;
CREATE TABLE app.t (id serial PRIMARY KEY, n integer);
CREATE TABLE app.gone (id bigserial PRIMARY KEY);
ALTER SEQUENCE app.s2 OWNED BY app.t.n;
COMMENT ON SEQUENCE app.s1 IS 'one';
-- @b
CREATE SEQUENCE app.s2 AS bigint START 7 INCREMENT 3 MINVALUE 2 MAXVALUE 900 CACHE 4 CYCLE;
CREATE SEQUENCE app.s3;
CREATE TABLE app.t (id serial PRIMARY KEY, n integer, m bigint DEFAULT nextval('app.s3'));
CREATE TABLE app.fresh (id smallserial, x integer);
ALTER SEQUENCE app.s3 OWNED BY app.t.m;
COMMENT ON SEQUENCE app.s2 IS 'two';
