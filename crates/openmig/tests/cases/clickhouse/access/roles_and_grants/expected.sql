-- Revoke on "dialo".* from "reader"
REVOKE INSERT ON `dialo`.* FROM `reader`;
-- Create role "gateway"
CREATE ROLE IF NOT EXISTS `gateway` IN `local_directory`;
-- Grant on "dialo"."events" to "gateway"
GRANT ALL ON `dialo`.`events` TO `gateway` WITH GRANT OPTION;
-- Grant on "dialo".* to "reader"
GRANT SELECT ON `dialo`.* TO `reader`;
-- Drop role "legacy"
DROP ROLE IF EXISTS `legacy`;
