-- Create user "app_svc"
CREATE USER IF NOT EXISTS `app_svc` IDENTIFIED WITH sha256_hash BY '2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b' IN `local_directory`;
-- Grant on "shop"."events" to "app_svc"
GRANT INSERT ON `shop`.`events` TO `app_svc`;
-- Grant role "reader" to "app_svc"
GRANT `reader` TO `app_svc`;
-- Set hosts, default roles and default database of user "app_svc"
ALTER USER `app_svc` HOST IP '10.0.0.0/8' DEFAULT ROLE `reader` DEFAULT DATABASE `shop` GRANTEES ANY;
-- Drop user "old_svc"
DROP USER IF EXISTS `old_svc`;
