-- A primary key under another name: the old one is dropped first (a table has one primary key).
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (id integer, CONSTRAINT t_old_pk PRIMARY KEY (id));
-- @b
CREATE TABLE app.t (id integer, CONSTRAINT t_new_pk PRIMARY KEY (id));
