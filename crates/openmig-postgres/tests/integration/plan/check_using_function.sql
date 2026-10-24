-- A check constraint and a default call a function whose body reads the table: the function is
-- created first (its body is not checked), then the table.
-- @both
CREATE SCHEMA app;
-- @a
-- @b
SET check_function_bodies = off;
CREATE FUNCTION app.next_code() RETURNS integer LANGUAGE sql AS $$ SELECT coalesce(max(code), 0) + 1 FROM app.items $$;
CREATE FUNCTION app.valid(c integer) RETURNS boolean LANGUAGE sql IMMUTABLE AS $$ SELECT c > 0 $$;
CREATE TABLE app.items (code integer DEFAULT app.next_code() CHECK (app.valid(code)));
