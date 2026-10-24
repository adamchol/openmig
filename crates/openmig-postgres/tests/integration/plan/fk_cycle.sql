-- Tables referencing each other are created with the cycle's foreign keys added afterwards,
-- and dropped after their foreign keys.
-- expect: ALTER TABLE
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.keep (id integer PRIMARY KEY);
-- @b
CREATE TABLE app.keep (id integer PRIMARY KEY);
CREATE TABLE app.a (id integer PRIMARY KEY, b_id integer, self_id integer REFERENCES app.a (id));
CREATE TABLE app.b (id integer PRIMARY KEY, a_id integer REFERENCES app.a (id), keep_id integer REFERENCES app.keep (id));
ALTER TABLE app.a ADD CONSTRAINT a_b_fk FOREIGN KEY (b_id) REFERENCES app.b (id);
CREATE TABLE app.c (id integer PRIMARY KEY, b_id integer REFERENCES app.b (id));
