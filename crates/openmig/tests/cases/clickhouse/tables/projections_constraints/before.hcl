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
  settings = { merge_with_ttl_timeout = 3600 }
  comment  = "events"

  projection "by_kind" {
    query = "SELECT kind, count() GROUP BY kind"
  }
  constraint "id_positive" {
    check = "id > 0"
  }
}
