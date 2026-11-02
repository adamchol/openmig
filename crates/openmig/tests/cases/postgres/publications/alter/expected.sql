-- Change the tables of publication "app_cdc"
ALTER PUBLICATION "app_cdc" DROP TABLE "app"."grants";
-- Change the tables of publication "app_cdc"
ALTER PUBLICATION "app_cdc" ADD TABLE "app"."grants" ("subject"), TABLE "app"."sessions";
-- Set the options of publication "app_cdc"
ALTER PUBLICATION "app_cdc" SET (publish = 'insert', publish_via_partition_root = false);
