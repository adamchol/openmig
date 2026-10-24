database "analytics" {}

table "analytics.events" {
  engine = MergeTree
  column "a" {
    type = UInt32
  }
  column "b" {
    type = UInt32
  }
  order_by = [column.a]
}
