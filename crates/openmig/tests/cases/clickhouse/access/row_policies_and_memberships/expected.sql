-- Create role "auditor"
CREATE ROLE IF NOT EXISTS `auditor` IN `replicated`;
-- Replace row policy "tenant_a" on "app"."events"
CREATE ROW POLICY OR REPLACE `tenant_a` ON `app`.`events` IN `local_directory` FOR SELECT USING tenant IN ('a', 'b') AS RESTRICTIVE TO ALL EXCEPT `admin`;
-- Grant role "auditor" to "admin"
GRANT `auditor` TO `admin`;
-- Grant role "reader" to "admin"
GRANT `reader` TO `admin` WITH ADMIN OPTION;
