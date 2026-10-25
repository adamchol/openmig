-- Revoke on "shop".* from "reader"
REVOKE INSERT ON `shop`.* FROM `reader`;
-- Create role "gateway"
CREATE ROLE IF NOT EXISTS `gateway` IN `local_directory`;
-- Grant on "shop"."events" to "gateway"
GRANT ALL ON `shop`.`events` TO `gateway` WITH GRANT OPTION;
-- Grant on "shop".* to "reader"
GRANT SELECT ON `shop`.* TO `reader`;
-- Drop role "legacy"
DROP ROLE IF EXISTS `legacy`;
