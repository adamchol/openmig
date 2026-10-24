schema "app" {}

enum "app.status" {
  values = ["active", "deleted"]
}

table "app.users" {
  column "status" {
    type    = enum.app.status
    null    = false
    default = "active"
  }
}
