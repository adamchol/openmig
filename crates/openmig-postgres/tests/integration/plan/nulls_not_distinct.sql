-- NULLS NOT DISTINCT unique constraints and indexes (PostgreSQL 15+).
-- min_major: 15
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (id integer, email text, CONSTRAINT t_id_key UNIQUE (id));
CREATE UNIQUE INDEX t_email_idx ON app.t (lower(email));
-- @b
CREATE TABLE app.t (id integer, email text, CONSTRAINT t_id_key UNIQUE NULLS NOT DISTINCT (id));
CREATE UNIQUE INDEX t_email_idx ON app.t (lower(email)) NULLS NOT DISTINCT;
