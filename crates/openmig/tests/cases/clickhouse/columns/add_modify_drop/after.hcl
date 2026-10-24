database "analytics" {}

table "analytics.events" {
  engine = MergeTree
  column "timestamp" {
    type = DateTime64(3)
  }
  column "kind" {
    type    = LowCardinality(String)
    comment = "Event kind"
  }
  column "user_id" {
    type    = UInt64
    default = 0
  }
  column "payload" {
    type  = String
    codec = [ZSTD(3)]
  }
  order_by = [column.timestamp]
}
