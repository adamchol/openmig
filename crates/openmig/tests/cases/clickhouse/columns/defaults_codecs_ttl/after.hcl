database "app" {}

table "app.events" {
  engine = MergeTree
  column "ts" {
    type = DateTime
  }
  column "a" {
    type         = String
    materialized = "concat('y', toString(ts))"
  }
  column "b" {
    type  = String
    alias = "toString(ts)"
  }
  column "c" {
    type = UInt64
  }
  column "d" {
    type  = UInt32
    codec = [Delta(4), ZSTD(1)]
    ttl   = "ts + toIntervalDay(7)"
    default = sql("toUInt32(1)")
  }
  order_by = [column.ts]
}
