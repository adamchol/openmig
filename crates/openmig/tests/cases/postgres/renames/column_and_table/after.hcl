schema "app" {}

table "app.people" {
  renamed_from = "app.users"
  column "id" {
    type = integer
    null = false
  }
  column "full_name" {
    type         = text
    renamed_from = "name"
  }
}
