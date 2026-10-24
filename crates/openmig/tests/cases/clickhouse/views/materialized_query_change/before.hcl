database "app" {}

table "app.src" {
  engine = MergeTree
  column "x" {
    type = UInt32
  }
  order_by = [column.x]
}

table "app.dst" {
  engine = MergeTree
  column "x" {
    type = UInt32
  }
  order_by = [column.x]
}

materialized_view "app.copy" {
  to = table.app.dst
  as = "SELECT x FROM app.src"
}
