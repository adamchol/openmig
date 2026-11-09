schema "app" {}

table "app.recordings" {
  column "id" {
    type = serial
    null = false
  }
}
