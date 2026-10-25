schema "app" {}

table "app.events" {
  column "id" {
    type = bigint
    null = false
  }
  column "at" {
    type = date
    null = false
  }
  partition_by = "RANGE (at)"
}

table "app.events_2026" {
  partition_of    = table.app.events
  partition_bound = "FOR VALUES FROM ('2026-01-01') TO ('2027-01-01')"
}
