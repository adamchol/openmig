-- With concurrent_indexes, indexes on existing tables are built and dropped concurrently and
-- the migration runs outside a transaction; indexes of new tables are not.
-- concurrent_indexes
-- no_transaction
-- expect: DROP INDEX CONCURRENTLY IF EXISTS "app"."t_email_idx"
-- expect: CREATE INDEX CONCURRENTLY "t_email_idx" ON "app"."t"
-- expect: DROP INDEX CONCURRENTLY "app"."t_old_idx"
-- reject: CONCURRENTLY "n_idx"
-- @both
CREATE SCHEMA app;
CREATE TABLE app.t (id integer PRIMARY KEY, email text, status text);
-- @a
CREATE INDEX t_old_idx ON app.t (status);
-- @b
CREATE INDEX t_email_idx ON app.t (lower(email)) WHERE status IS NOT NULL;
CREATE TABLE app.n (id integer, v text);
CREATE INDEX n_idx ON app.n (v);
