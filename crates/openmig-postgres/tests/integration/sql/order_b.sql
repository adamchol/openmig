CREATE SCHEMA ord;
CREATE TYPE ord.color AS ENUM ('green');
ALTER TYPE ord.color ADD VALUE 'red' BEFORE 'green';
ALTER TYPE ord.color ADD VALUE 'blue' AFTER 'green';
CREATE SEQUENCE ord.counter;
ALTER SEQUENCE ord.counter START 7 INCREMENT 3 CACHE 2;
CREATE TABLE ord.child (
  id integer,
  parent_id integer,
  dropped text,
  note text
);
ALTER TABLE ord.child DROP COLUMN dropped;
CREATE TABLE ord.parent (
  id integer NOT NULL,
  name text,
  color ord.color
);
ALTER TABLE ord.parent ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY (START WITH 5);
ALTER TABLE ord.parent ALTER COLUMN name SET DEFAULT 'x';
ALTER TABLE ord.parent ALTER COLUMN name SET NOT NULL;
ALTER TABLE ord.parent ADD CONSTRAINT parent_name_len CHECK (length(name) < 100);
ALTER TABLE ord.parent ADD CONSTRAINT parent_name_key UNIQUE (name);
ALTER TABLE ord.parent ADD PRIMARY KEY (id);
ALTER TABLE ord.child ADD PRIMARY KEY (id);
INSERT INTO ord.parent (name) VALUES ('a');
ALTER TABLE ord.child ADD CONSTRAINT child_parent_fk FOREIGN KEY (parent_id) REFERENCES ord.parent (id) ON DELETE CASCADE;
CREATE INDEX child_parent_idx ON ord.child (parent_id);
COMMENT ON COLUMN ord.child.note IS 'free text';
COMMENT ON TABLE ord.parent IS 'parents';
COMMENT ON SCHEMA ord IS 'ordering test';
CREATE FUNCTION ord.name_of(p integer) RETURNS text LANGUAGE sql STABLE AS $$ SELECT 'none' $$;
CREATE OR REPLACE FUNCTION ord.name_of(p integer) RETURNS text LANGUAGE sql STABLE AS $$ SELECT name FROM ord.parent WHERE id = p $$;
REVOKE ALL ON FUNCTION ord.name_of(integer) FROM PUBLIC;
CREATE VIEW ord.named AS SELECT c.id, 'x'::text AS name FROM ord.child c;
CREATE OR REPLACE VIEW ord.named AS SELECT c.id, ord.name_of(c.parent_id) AS name FROM ord.child c;
CREATE POLICY p2 ON ord.child FOR UPDATE TO __R__a, __R__b USING (note IS NOT NULL);
CREATE POLICY p1 ON ord.child FOR SELECT TO __R__a USING (true);
ALTER TABLE ord.child ENABLE ROW LEVEL SECURITY;
ALTER TABLE ord.child DISABLE ROW LEVEL SECURITY;
ALTER TABLE ord.child ENABLE ROW LEVEL SECURITY;
GRANT ALL ON ord.parent TO __R__a;
REVOKE ALL ON ord.parent FROM __R__a;
GRANT INSERT ON ord.parent TO __R__a;
GRANT SELECT ON ord.parent TO __R__a;
GRANT SELECT ON ord.named TO __R__b;
