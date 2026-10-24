schema "app" {}

table "app.metrics" {
  column "value" {
    type = integer
  }
}

view "app.metric_values" {
  as = "SELECT value FROM app.metrics"
}
