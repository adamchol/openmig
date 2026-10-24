schema "app" {}

table "app.users" {
  column "id" {
    type = integer
    null = false
  }
  column "name" {
    type = text
  }
}
