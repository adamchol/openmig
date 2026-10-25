-- Create role "svc_admin"
DO $$BEGIN IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'svc_admin') THEN CREATE ROLE "svc_admin" WITH CREATEROLE NOINHERIT VALID UNTIL '2030-01-01 00:00:00+00'; END IF; END$$;
-- Alter role "svc_api"
ALTER ROLE "svc_api" WITH CONNECTION LIMIT 10;
-- Set membership of role "svc_api" in role "svc_base"
GRANT "svc_base" TO "svc_api" WITH ADMIN FALSE, INHERIT TRUE, SET TRUE;
-- Set comment on role "svc_base"
COMMENT ON ROLE "svc_base" IS 'Base role';
-- Drop role "svc_old"
DROP ROLE "svc_old";
