-- Tables and columns: add, drop, type, nullability, default, collation, comments.
-- expect: ALTER TABLE "app"."t" ADD COLUMN "added" text
-- expect: DROP TABLE "app"."gone"
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (
  id integer NOT NULL,
  name varchar(10),
  note text DEFAULT 'x',
  dropped boolean,
  code text
);
CREATE TABLE app.gone (id int);
COMMENT ON COLUMN app.t.name IS 'old';
-- @b
CREATE TABLE app.t (
  id bigint,
  name text NOT NULL,
  note text DEFAULT 'y',
  code text COLLATE "C",
  added text DEFAULT now()::text
);
CREATE TABLE app.fresh (id int PRIMARY KEY, label text NOT NULL DEFAULT '');
COMMENT ON TABLE app.t IS 'the table';
COMMENT ON COLUMN app.t.added IS 'new';
