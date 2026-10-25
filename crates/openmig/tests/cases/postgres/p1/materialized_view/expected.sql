-- Drop materialized view "app"."order_totals"
DROP MATERIALIZED VIEW "app"."order_totals";
-- Create materialized view "app"."order_totals"
CREATE MATERIALIZED VIEW "app"."order_totals" AS SELECT count(*) AS n,
    sum(total) AS total
   FROM app.orders WITH DATA;
-- Create index "order_totals_n_idx" on "app"."order_totals"
CREATE INDEX "order_totals_n_idx" ON "app"."order_totals" USING btree ("n");
-- Set comment on materialized view "app"."order_totals"
COMMENT ON MATERIALIZED VIEW "app"."order_totals" IS 'Totals';
