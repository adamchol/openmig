-- Views: replaced in place when columns are only appended, recreated (with dependent views)
-- otherwise; options, comments on views and view columns, grants.
-- roles: omp_reader
-- expect: CREATE OR REPLACE VIEW "app"."appended"
-- expect: DROP VIEW "app"."on_renamed"
-- @both
CREATE SCHEMA app;
CREATE TABLE app.t (id integer PRIMARY KEY, name text, extra text);
-- @a
CREATE VIEW app.appended AS SELECT id FROM app.t;
CREATE VIEW app.renamed AS SELECT id, name FROM app.t;
CREATE VIEW app.on_renamed AS SELECT id FROM app.renamed;
CREATE VIEW app.opts AS SELECT id, name FROM app.t;
CREATE VIEW app.gone AS SELECT 1 AS one;
COMMENT ON VIEW app.opts IS 'options';
GRANT SELECT ON app.opts TO omp_reader;
-- @b
CREATE VIEW app.appended AS SELECT id, name FROM app.t WHERE id > 0;
CREATE VIEW app.renamed AS SELECT id, name AS label FROM app.t;
CREATE VIEW app.on_renamed AS SELECT id FROM app.renamed;
CREATE VIEW app.opts WITH (security_barrier = true) AS SELECT id, name FROM app.t WITH LOCAL CHECK OPTION;
CREATE VIEW app.fresh AS SELECT count(*) AS n FROM app.t;
COMMENT ON COLUMN app.opts.name IS 'the name';
GRANT SELECT (id) ON app.opts TO omp_reader;
GRANT SELECT ON app.fresh TO omp_reader;
