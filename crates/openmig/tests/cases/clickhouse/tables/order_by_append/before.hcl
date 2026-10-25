database "app" {}

table "app.events" {
  engine = MergeTree
  column "a" {
    type = UInt32
  }
  column "b" {
    type = String
  }
  order_by = [column.a]
}
