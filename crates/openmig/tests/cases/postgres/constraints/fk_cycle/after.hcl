schema "app" {}

table "app.a" {
  column "id" {
    type = integer
    null = false
  }
  column "b_id" {
    type = integer
  }
  primary_key {
    columns = [column.id]
  }
  foreign_key "a_b_fk" {
    columns    = [column.b_id]
    references = [table.app.b.column.id]
  }
}

table "app.b" {
  column "id" {
    type = integer
    null = false
  }
  column "a_id" {
    type = integer
  }
  primary_key {
    columns = [column.id]
  }
  foreign_key "b_a_fk" {
    columns    = [column.a_id]
    references = [table.app.a.column.id]
    on_delete  = SET_NULL
  }
}
