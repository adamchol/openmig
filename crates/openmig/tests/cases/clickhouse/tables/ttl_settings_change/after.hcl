database "analytics" {}

table "analytics.events" {
  engine = MergeTree
  column "timestamp" {
    type = DateTime
  }
  order_by = [column.timestamp]
  ttl      = "timestamp + toIntervalDay(90)"
  settings = { index_granularity = 8192, ttl_only_drop_parts = 1 }
}
