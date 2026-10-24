database "analytics" {}

table "analytics.events" {
  engine = MergeTree
  column "timestamp" {
    type = DateTime64(3)
  }
  column "kind" {
    type = LowCardinality(String)
  }
  column "payload" {
    type = String
  }
  order_by     = [column.timestamp]
  partition_by = "toYYYYMM(timestamp)"
}
