-- A table moved to another schema and renamed keeps its data, indexes and owned sequences; the
-- foreign key referencing it follows.
-- rename: table app.people crm.users
-- expect: SET SCHEMA "crm"
-- expect: RENAME TO "users"
-- reject: DROP TABLE
-- reject: SET DEFAULT
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.people (id serial PRIMARY KEY, name text);
CREATE INDEX people_name_idx ON app.people (name);
CREATE TABLE app.notes (id integer, person_id integer REFERENCES app.people (id));
-- @b
CREATE SCHEMA crm;
CREATE TABLE crm.users (id integer PRIMARY KEY, name text, email text);
CREATE SEQUENCE crm.people_id_seq AS integer OWNED BY crm.users.id;
ALTER TABLE crm.users ALTER COLUMN id SET DEFAULT nextval('crm.people_id_seq');
ALTER INDEX crm.users_pkey RENAME TO people_pkey;
CREATE INDEX people_name_idx ON crm.users (name);
CREATE TABLE app.notes (id integer, person_id integer REFERENCES crm.users (id));
