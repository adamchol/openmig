-- Modify query of materialized view "app"."copy"
ALTER TABLE `app`.`copy` MODIFY QUERY
SELECT x * 2 AS x FROM app.src;
