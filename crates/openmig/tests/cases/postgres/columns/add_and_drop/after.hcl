schema "app" {}

table "app.users" {
  column "id" {
    type = bigint
    null = false
  }
  column "name" {
    type    = varchar(200)
    default = "anonymous"
  }
  column "created_at" {
    type    = timestamptz
    null    = false
    default = sql("now()")
  }
  primary_key {
    columns = [column.id]
  }
}
