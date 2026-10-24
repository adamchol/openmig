-- Removing an enum value that a column uses is unsupported.
-- unsupported
-- no_reverse
-- @both
CREATE SCHEMA app;
-- @a
CREATE TYPE app.status AS ENUM ('active', 'disabled');
CREATE TABLE app.t (s app.status);
-- @b
CREATE TYPE app.status AS ENUM ('active');
CREATE TABLE app.t (s app.status);
