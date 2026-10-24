schema "app" {}

table "app.events" {
  column "id" {
    type = uuid
    null = false
    default = sql("gen_random_uuid()")
  }
  column "tenant_id" {
    type = integer
    null = false
  }
  primary_key {
    columns = [column.id]
  }
}
