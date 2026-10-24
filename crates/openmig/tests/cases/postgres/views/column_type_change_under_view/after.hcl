schema "app" {}

table "app.metrics" {
  column "value" {
    type = bigint
  }
}

view "app.metric_values" {
  as = "SELECT value FROM app.metrics"
}
