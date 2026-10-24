database "otel" {}

table "otel.traces" {
  engine = MergeTree
  column "TraceId" {
    type = String
  }
  column "Timestamp" {
    type = DateTime64(9)
  }
  order_by = [column.TraceId, column.Timestamp]
}

table "otel.trace_ranges" {
  engine = ReplacingMergeTree
  column "TraceId" {
    type = String
  }
  column "Start" {
    type = DateTime64(9)
  }
  column "End" {
    type = DateTime64(9)
  }
  order_by = [column.TraceId]
}

materialized_view "otel.trace_ranges_mv" {
  to = table.otel.trace_ranges
  as = "SELECT TraceId, min(Timestamp) AS Start, max(Timestamp) AS End FROM otel.traces WHERE TraceId != '' GROUP BY TraceId"
}
