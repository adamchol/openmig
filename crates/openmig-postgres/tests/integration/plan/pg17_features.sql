-- MAINTAIN privileges (PostgreSQL 17+).
-- min_major: 17
-- roles: omp_maint
-- expect: GRANT MAINTAIN ON TABLE "app"."t" TO "omp_maint"
-- @both
CREATE SCHEMA app;
CREATE TABLE app.t (id integer);
-- @a
-- @b
GRANT MAINTAIN ON app.t TO omp_maint;
