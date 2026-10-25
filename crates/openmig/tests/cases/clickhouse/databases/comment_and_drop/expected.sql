-- Create database "fresh"
CREATE DATABASE IF NOT EXISTS `fresh` ENGINE = Atomic;
-- Comment on database "keep"
ALTER DATABASE `keep` MODIFY COMMENT 'new';
-- Drop database "gone"
DROP DATABASE IF EXISTS `gone` SYNC;
