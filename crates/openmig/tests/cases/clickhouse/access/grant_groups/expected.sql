-- Revoke grant option on *.* from "web"
REVOKE GRANT OPTION FOR SELECT ON *.* FROM `web`;
-- Revoke on "app".* from "web"
REVOKE ACCESS MANAGEMENT, ALTER, BACKUP, CHECK, CREATE, DROP, OPTIMIZE, SHOW, SYSTEM, TRUNCATE, UNDROP TABLE ON `app`.* FROM `web`;
-- Grant on "app".* to "web"
GRANT ALTER UPDATE ON `app`.* TO `web`;
-- Grant on "app"."events" to "web"
GRANT SELECT(`id`) ON `app`.`events` TO `web` WITH GRANT OPTION;
