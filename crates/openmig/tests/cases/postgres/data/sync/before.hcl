schema "app" {}

table "app.statuses" {
  column "id" {
    type = integer
    null = false
  }
  column "name" {
    type = text
    null = false
  }
  primary_key {
    columns = [column.id]
  }
}

data "app.statuses" {
  rows = [
    { id = 1, name = "active" },
    { id = 2, name = "inactive" },
    { id = 3, name = "deleted" },
  ]
}
