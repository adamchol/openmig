-- Extensions: created before the columns that use their types, dropped after them.
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (id integer);
-- @b
CREATE EXTENSION IF NOT EXISTS citext WITH SCHEMA public;
CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA app;
CREATE TABLE app.t (id integer, email public.citext);
CREATE INDEX t_email_trgm ON app.t USING gin ((email::text) app.gin_trgm_ops);
COMMENT ON EXTENSION citext IS 'emails';
