-- Partial, expression, covering, unique and NULLS NOT DISTINCT indexes; changes recreate them.
-- @both
CREATE SCHEMA app;
-- @a
CREATE TABLE app.t (id integer PRIMARY KEY, email text, status text, tags text[], score int);
CREATE INDEX t_email_idx ON app.t (email);
CREATE INDEX t_partial_idx ON app.t (status) WHERE status <> 'x';
CREATE INDEX t_tags_idx ON app.t USING gin (tags);
CREATE INDEX t_gone_idx ON app.t (score);
COMMENT ON INDEX app.t_tags_idx IS 'tags';
-- @b
CREATE TABLE app.t (id integer PRIMARY KEY, email text, status text, tags text[], score int);
CREATE UNIQUE INDEX t_email_idx ON app.t (lower(email)) NULLS NOT DISTINCT;
CREATE INDEX t_partial_idx ON app.t (status, score DESC NULLS LAST) INCLUDE (email) WHERE status <> 'y';
CREATE INDEX t_tags_idx ON app.t USING gin (tags);
CREATE INDEX t_new_idx ON app.t (email text_pattern_ops, (score + 1)) WITH (fillfactor = 70);
COMMENT ON INDEX app.t_tags_idx IS 'tag search';
