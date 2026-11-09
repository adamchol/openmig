schema "app" {}

table "app.recordings" {
  column "id" {
    type = uuid
    null = false
  }
}
