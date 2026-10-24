schema "app" {}

table "app.users" {
  column "id" {
    type = bigint
    null = false
  }
  column "legacy" {
    type = text
  }
  primary_key {
    columns = [column.id]
  }
}
