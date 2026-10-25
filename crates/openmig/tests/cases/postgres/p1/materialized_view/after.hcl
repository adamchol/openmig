schema "app" {}

table "app.orders" {
  column "id" {
    type = integer
    null = false
  }
  column "total" {
    type = numeric
    null = false
  }
}

materialized_view "app.order_totals" {
  as = "SELECT count(*) AS n, sum(total) AS total FROM app.orders"
  index "order_totals_n_idx" {
    columns = ["n"]
  }
  comment = "Totals"
}
