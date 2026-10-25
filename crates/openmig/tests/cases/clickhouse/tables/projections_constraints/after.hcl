database "app" {}

table "app.events" {
  engine = MergeTree
  column "id" {
    type = UInt64
  }
  column "kind" {
    type = String
  }
  order_by  = [column.id]
  sample_by = "id"
  comment   = "all events"

  projection "sorted" {
    query = "SELECT * ORDER BY kind"
  }
  constraint "id_positive" {
    check = "id > 1"
  }
  constraint "kind_known" {
    assume = "kind != ''"
  }
}
