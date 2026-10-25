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
  as = "SELECT count(*) AS n FROM app.orders"
}
