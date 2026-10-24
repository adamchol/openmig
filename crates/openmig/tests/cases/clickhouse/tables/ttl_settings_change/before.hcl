database "analytics" {}

table "analytics.events" {
  engine = MergeTree
  column "timestamp" {
    type = DateTime
  }
  order_by = [column.timestamp]
  ttl      = "timestamp + toIntervalDay(30)"
}
