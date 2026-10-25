database "analytics" {}

table "analytics.page_views" {
  engine = MergeTree
  column "timestamp" {
    type  = DateTime64(3)
    codec = [Delta(8), ZSTD(1)]
  }
  column "path" {
    type = LowCardinality(String)
  }
  column "customer_id" {
    type = Nullable(UInt64)
  }
  order_by     = [column.path, column.timestamp]
  partition_by = "toYYYYMM(timestamp)"
  ttl          = "toDateTime(timestamp) + toIntervalDay(90)"
}

table "analytics.daily_views" {
  engine = SummingMergeTree
  column "day" {
    type = Date
  }
  column "path" {
    type = LowCardinality(String)
  }
  column "views" {
    type = UInt64
  }
  order_by = [column.day, column.path]
}

materialized_view "analytics.daily_views_mv" {
  to = table.analytics.daily_views
  as = "SELECT toDate(timestamp) AS day, path, count() AS views FROM analytics.page_views GROUP BY day, path"
}
