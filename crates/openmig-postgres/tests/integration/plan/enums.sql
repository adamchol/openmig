-- Enum values are added in place, in declared order; a value used by the same plan forces a
-- migration outside a transaction.
-- no_transaction
-- no_reverse
-- expect: ADD VALUE 'new' BEFORE 'active'
-- expect: ADD VALUE 'archived' AFTER 'disabled'
-- @both
CREATE SCHEMA app;
-- @a
CREATE TYPE app.status AS ENUM ('active', 'disabled');
CREATE TABLE app.t (id integer, s app.status DEFAULT 'active');
-- @b
CREATE TYPE app.status AS ENUM ('new', 'active', 'paused', 'disabled', 'archived');
CREATE TABLE app.t (id integer, s app.status DEFAULT 'new');
CREATE TYPE app.color AS ENUM ('red');
COMMENT ON TYPE app.status IS 'status';
