schema "app" {}

table "app.hot" {
  unlogged       = true
  storage_params = { fillfactor = 70, autovacuum_enabled = false }
  column "k" {
    type = text
    null = false
  }
  column "v" {
    type    = text
    storage = EXTERNAL
  }
  primary_key {
    columns = [column.k]
  }
}
