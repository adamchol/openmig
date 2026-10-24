-- Data-managed tables: rows inserted, updated and deleted by key, parents before children on
-- insert and children before parents on delete; insert-only tables never lose rows.
-- data: app.kinds sync
-- data: app.items sync id kind label
-- data: app.seen insert
-- expect: DELETE FROM "app"."items"
-- @both
CREATE SCHEMA app;
CREATE TABLE app.kinds (code text PRIMARY KEY, label text);
CREATE TABLE app.items (id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY, kind text REFERENCES app.kinds (code), label text, note text);
CREATE TABLE app.seen (k integer PRIMARY KEY, v text);
-- @a
INSERT INTO app.kinds VALUES ('a', 'A'), ('old', 'Old');
INSERT INTO app.items (id, kind, label, note) OVERRIDING SYSTEM VALUE VALUES (1, 'a', 'one', 'kept'), (2, 'old', 'two', 'x');
INSERT INTO app.seen VALUES (1, 'a');
-- @b
INSERT INTO app.kinds VALUES ('a', 'Alpha'), ('b', 'B');
INSERT INTO app.items (id, kind, label, note) OVERRIDING SYSTEM VALUE VALUES (1, 'b', 'one', 'kept'), (3, 'b', 'it''s three', NULL);
INSERT INTO app.seen VALUES (1, 'a'), (2, NULL);
