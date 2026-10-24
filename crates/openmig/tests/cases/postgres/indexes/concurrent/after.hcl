schema "app" {}

table "app.users" {
  column "id" {
    type = integer
    null = false
  }
  column "email" {
    type = text
  }
  index "users_email_idx" {
    columns = [column.email]
  }
}
