-- Replace view "app"."active_users"
CREATE OR REPLACE VIEW "app"."active_users" WITH (security_invoker = true) AS SELECT id,
    email
   FROM app.users
  WHERE active;
