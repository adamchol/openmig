database "analytics" {
  comment = "Event store"
}

table "analytics.logs" {
  engine = ReplacingMergeTree(updated_at)
  column "service" {
    type  = LowCardinality(String)
    codec = [ZSTD(1)]
  }
  column "ts" {
    type  = DateTime64(9)
    codec = [Delta(8), ZSTD(1)]
  }
  column "day" {
    type         = Date
    materialized = "toDate(ts)"
  }
  column "body" {
    type    = String
    default = ""
    comment = "Raw log line"
  }
  column "attrs" {
    type = Map(LowCardinality(String), String)
  }
  column "updated_at" {
    type = DateTime
  }
  primary_key  = [column.service, column.day]
  order_by     = [column.service, column.day, column.ts]
  partition_by = "toYYYYMM(day)"
  ttl          = "day + toIntervalDay(30)"
  settings     = { index_granularity = 4096, ttl_only_drop_parts = 1 }
  index "body_tokens" {
    expr        = "body"
    type        = tokenbf_v1(32768, 3, 0)
    granularity = 4
  }
  index "attr_keys" {
    expr = "mapKeys(attrs)"
    type = bloom_filter(0.01)
  }
  comment = "Application logs"
}
