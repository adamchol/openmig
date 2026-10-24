schema "app" {}

table "app.users" {
  column "id" {
    type     = bigint
    identity = BY_DEFAULT
  }
  column "email" {
    type = text
    null = false
  }
  primary_key {
    columns = [column.id]
  }
}
