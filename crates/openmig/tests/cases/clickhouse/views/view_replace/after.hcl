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
  as           = "SELECT kind, count() AS n FROM app.events GROUP BY kind"
  sql_security = INVOKER
  comment      = "Event kinds"
}

view "app.recent" {
  as = <<-SQL
    SELECT id, kind
    FROM app.events
    WHERE id > 100
  SQL
}
