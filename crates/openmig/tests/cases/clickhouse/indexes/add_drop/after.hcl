database "analytics" {}

table "analytics.events" {
  engine = MergeTree
  column "timestamp" {
    type = DateTime64(3)
  }
  column "call_id" {
    type = String
  }
  order_by = [column.timestamp]
  index "call_id_set" {
    expr = "call_id"
    type = set(1024)
  }
}
