CREATE SCHEMA ord;
COMMENT ON SCHEMA ord IS 'ordering test';
CREATE TYPE ord.color AS ENUM ('red', 'green', 'blue');
CREATE SEQUENCE ord.counter AS bigint START 7 INCREMENT 3 CACHE 2;
CREATE TABLE ord.parent (
  id integer GENERATED ALWAYS AS IDENTITY (START WITH 5) PRIMARY KEY,
  name text NOT NULL DEFAULT 'x',
  color ord.color,
  CONSTRAINT parent_name_key UNIQUE (name),
  CONSTRAINT parent_name_len CHECK (length(name) < 100)
);
CREATE TABLE ord.child (
  id integer PRIMARY KEY,
  parent_id integer CONSTRAINT child_parent_fk REFERENCES ord.parent (id) ON DELETE CASCADE,
  note text
);
CREATE INDEX child_parent_idx ON ord.child (parent_id);
INSERT INTO ord.parent (name) VALUES ('a');
COMMENT ON TABLE ord.parent IS 'parents';
COMMENT ON COLUMN ord.child.note IS 'free text';
CREATE FUNCTION ord.name_of(p integer) RETURNS text LANGUAGE sql STABLE AS $$ SELECT name FROM ord.parent WHERE id = p $$;
REVOKE EXECUTE ON FUNCTION ord.name_of(integer) FROM PUBLIC;
CREATE VIEW ord.named AS SELECT c.id, ord.name_of(c.parent_id) AS name FROM ord.child c;
ALTER TABLE ord.child ENABLE ROW LEVEL SECURITY;
CREATE POLICY p1 ON ord.child FOR SELECT TO __R__a USING (true);
CREATE POLICY p2 ON ord.child FOR UPDATE TO __R__b, __R__a USING (note IS NOT NULL);
GRANT SELECT, INSERT ON ord.parent TO __R__a;
GRANT SELECT ON ord.named TO __R__b;
