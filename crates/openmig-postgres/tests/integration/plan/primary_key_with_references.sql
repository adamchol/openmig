-- A primary key that changes while foreign keys reference it: the foreign keys are rebuilt.
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.p (id integer, k integer NOT NULL, CONSTRAINT p_pkey PRIMARY KEY (id));
CREATE TABLE app.c (p_id integer REFERENCES app.p (id));
-- @b
CREATE TABLE app.p (id integer, k integer NOT NULL, CONSTRAINT p_pkey PRIMARY KEY (id) WITH (fillfactor = 90));
CREATE TABLE app.c (p_id integer REFERENCES app.p (id));
