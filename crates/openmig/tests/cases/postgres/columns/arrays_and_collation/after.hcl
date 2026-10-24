schema "app" {}

table "app.docs" {
  column "tags" {
    type    = "text[]"
    null    = false
    default = sql("'{}'::text[]")
  }
  column "matrix" {
    type = "integer[][]"
  }
  column "title" {
    type    = text
    collate = "C"
  }
  column "doc" {
    type = jsonb
  }
}
