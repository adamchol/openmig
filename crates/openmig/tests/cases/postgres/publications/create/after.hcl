schema "app" {}
schema "events" {}

table "app.grants" {
  column "subject" {
    type = text
    null = false
  }
  column "session_id" {
    type = uuid
    null = false
  }
  column "secret" {
    type = text
  }
  primary_key {
    columns = [column.subject, column.session_id]
  }
}

table "app.sessions" {
  column "id" {
    type = uuid
    null = false
  }
  primary_key {
    columns = [column.id]
  }
}

publication "app_cdc" {
  tables  = [table.app.sessions]
  publish = [INSERT, UPDATE, DELETE]
  comment = "read by the CDC reader"

  table "app.grants" {
    columns = ["subject", "session_id"]
    where   = "subject <> 'system'"
  }
}

publication "app_events" {
  schemas                    = [schema.events]
  publish_via_partition_root = true
}

publication "app_empty" {}
