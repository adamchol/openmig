schema "app" {}

schema "archive" {}

table "archive.logs" {
  renamed_from = "app.logs"
  column "line" {
    type = text
  }
}
