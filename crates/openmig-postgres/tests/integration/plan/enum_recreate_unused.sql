-- With allow_recreate, an enum whose values are removed or reordered is recreated together
-- with the function that uses it.
-- recreate
-- @both
CREATE SCHEMA app;
-- @a
CREATE TYPE app.status AS ENUM ('a', 'b', 'c');
CREATE FUNCTION app.f(s app.status) RETURNS text LANGUAGE sql AS $$ SELECT s::text $$;
-- @b
CREATE TYPE app.status AS ENUM ('c', 'a');
CREATE FUNCTION app.f(s app.status) RETURNS text LANGUAGE sql AS $$ SELECT s::text $$;
