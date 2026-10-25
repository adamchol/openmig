database "app" {}

table "app.src" {
  engine = MergeTree
  column "k" {
    type = UInt32
  }
  column "x" {
    type = UInt64
  }
  order_by = [column.k]
}

table "app.dst" {
  engine = MergeTree
  column "x" {
    type = UInt64
  }
  order_by = [column.x]
}

materialized_view "app.copy" {
  to = table.app.dst
  as = "SELECT x FROM app.src"
}
