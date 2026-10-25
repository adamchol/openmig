schema "app" {}

table "app.hot" {
  column "k" {
    type = text
    null = false
  }
  column "v" {
    type = text
  }
  primary_key {
    columns = [column.k]
  }
}
