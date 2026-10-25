database "app" {}

table "app.events" {
  engine = MergeTree
  column "day" {
    type = Date
  }
  column "kind" {
    type = String
  }
  order_by = [column.day]
}

table "app.daily" {
  engine = MergeTree
  column "day" {
    type = Date
  }
  column "n" {
    type = UInt64
  }
  order_by = [column.day]
}

materialized_view "app.kind_counts" {
  engine       = SummingMergeTree
  order_by     = ["kind"]
  partition_by = "tuple()"
  as           = "SELECT kind, count() AS n FROM app.events GROUP BY kind"
}

materialized_view "app.daily_refresh" {
  refresh = "EVERY 1 HOUR"
  to      = table.app.daily
  as      = "SELECT day, count() AS n FROM app.events GROUP BY day"
}
