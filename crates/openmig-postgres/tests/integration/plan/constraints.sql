-- Primary keys, foreign keys, unique, check and exclusion constraints: add, drop, change,
-- validate, comments.
-- expect: VALIDATE CONSTRAINT "t_amount_check"
-- @both
CREATE SCHEMA app;
CREATE EXTENSION IF NOT EXISTS btree_gist WITH SCHEMA public;
CREATE TABLE app.parent (id integer PRIMARY KEY, code text NOT NULL UNIQUE, alt integer NOT NULL);
-- @a
CREATE TABLE app.t (
  id integer NOT NULL,
  parent_id integer,
  amount integer,
  during tstzrange,
  CONSTRAINT t_pkey PRIMARY KEY (id),
  CONSTRAINT t_parent_fk FOREIGN KEY (parent_id) REFERENCES app.parent (id),
  CONSTRAINT t_amount_key UNIQUE (amount),
  CONSTRAINT t_old_check CHECK (amount > 0)
);
ALTER TABLE app.t ADD CONSTRAINT t_amount_check CHECK (amount < 1000) NOT VALID;
COMMENT ON CONSTRAINT t_pkey ON app.t IS 'pk';
-- @b
CREATE UNIQUE INDEX parent_alt_key ON app.parent (alt);
CREATE TABLE app.t (
  id integer NOT NULL,
  parent_id integer,
  amount integer,
  during tstzrange,
  CONSTRAINT t_pkey PRIMARY KEY (id, parent_id) DEFERRABLE,
  CONSTRAINT t_parent_fk FOREIGN KEY (parent_id) REFERENCES app.parent (id) ON DELETE CASCADE,
  CONSTRAINT t_code_fk FOREIGN KEY (amount) REFERENCES app.parent (alt) MATCH FULL DEFERRABLE INITIALLY DEFERRED,
  CONSTRAINT t_amount_key UNIQUE NULLS NOT DISTINCT (amount, id) INCLUDE (parent_id),
  CONSTRAINT t_amount_check CHECK (amount < 1000),
  CONSTRAINT t_no_overlap EXCLUDE USING gist (id WITH =, during WITH &&) WHERE (id > 0)
);
COMMENT ON CONSTRAINT t_pkey ON app.t IS 'composite pk';
COMMENT ON CONSTRAINT t_amount_check ON app.t IS 'bounded';
