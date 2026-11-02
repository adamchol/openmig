schema "app" {}

table "app.grants" {
  column "subject" {
    type = text
    null = false
  }
  column "session_id" {
    type = uuid
    null = false
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
  publish = [INSERT]

  table "app.grants" {
    columns = ["subject"]
  }
}
