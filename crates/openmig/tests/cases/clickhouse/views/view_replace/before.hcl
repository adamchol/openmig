database "app" {}

table "app.events" {
  engine = MergeTree
  column "id" {
    type = UInt64
  }
  column "kind" {
    type = String
  }
  order_by = [column.id]
}

view "app.kinds" {
  as = "SELECT DISTINCT kind FROM app.events"
}
