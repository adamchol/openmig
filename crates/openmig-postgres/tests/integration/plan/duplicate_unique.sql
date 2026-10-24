-- A unique constraint on the primary key's columns is added after CREATE TABLE, which would
-- silently drop it.
-- expect: ADD CONSTRAINT "t_same_key" UNIQUE
-- @both
CREATE SCHEMA app;
-- @a
-- @b
CREATE TABLE app.t (a integer, b integer, PRIMARY KEY (a, b));
ALTER TABLE app.t ADD CONSTRAINT t_same_key UNIQUE (a, b);
ALTER TABLE app.t ADD CONSTRAINT t_other_key UNIQUE (b);
ALTER TABLE app.t ADD CONSTRAINT t_other_again UNIQUE (b);
