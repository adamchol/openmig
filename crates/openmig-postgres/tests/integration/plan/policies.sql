-- Row level security and policies: ALTER POLICY for roles and expressions, drop and create when
-- the command, kind or a removed clause changes.
-- roles: omp_web, omp_admin
-- expect: ALTER POLICY "altered"
-- expect: FORCE ROW LEVEL SECURITY
-- @both
CREATE SCHEMA app;
CREATE TABLE app.t (id integer PRIMARY KEY, owner text);
CREATE TABLE app.u (id integer);
-- @a
ALTER TABLE app.t ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.u ENABLE ROW LEVEL SECURITY;
CREATE POLICY altered ON app.t FOR SELECT TO omp_web USING (true);
CREATE POLICY command ON app.t FOR UPDATE USING (owner = current_user);
CREATE POLICY clause ON app.t FOR UPDATE USING (true) WITH CHECK (owner IS NOT NULL);
CREATE POLICY gone ON app.t USING (false);
-- @b
ALTER TABLE app.t ENABLE ROW LEVEL SECURITY;
ALTER TABLE app.t FORCE ROW LEVEL SECURITY;
CREATE POLICY altered ON app.t FOR SELECT TO omp_web, omp_admin USING (owner = current_user);
CREATE POLICY command ON app.t AS RESTRICTIVE FOR DELETE USING (owner = current_user);
CREATE POLICY clause ON app.t FOR UPDATE USING (true);
CREATE POLICY fresh ON app.t FOR INSERT TO omp_admin WITH CHECK (EXISTS (SELECT 1 FROM app.u WHERE u.id = t.id));
COMMENT ON POLICY fresh ON app.t IS 'new';
