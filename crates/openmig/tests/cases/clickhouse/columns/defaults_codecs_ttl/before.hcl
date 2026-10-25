database "app" {}

table "app.events" {
  engine = MergeTree
  column "ts" {
    type = DateTime
  }
  column "a" {
    type    = String
    default = "x"
  }
  column "b" {
    type         = String
    materialized = "toString(ts)"
    codec        = [ZSTD(1)]
  }
  column "c" {
    type    = UInt64
    comment = "old"
    ttl     = "ts + toIntervalDay(1)"
  }
  column "d" {
    type = UInt32
  }
  order_by = [column.ts]
}
