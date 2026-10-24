schema "app" {}

table "app.users" {
  column "id" {
    type = integer
    null = false
  }
  column "email" {
    type = text
  }
  index "users_old_idx" {
    columns = [column.id]
  }
}
