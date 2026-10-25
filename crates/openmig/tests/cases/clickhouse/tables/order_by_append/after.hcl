database "app" {}

table "app.events" {
  engine = MergeTree
  column "a" {
    type = UInt32
  }
  column "b" {
    type = String
  }
  column "c" {
    type = UInt8
  }
  order_by    = [column.a, column.c]
  primary_key = [column.a]
}
