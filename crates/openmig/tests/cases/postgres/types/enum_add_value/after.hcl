schema "app" {}

enum "app.status" {
  values = ["pending", "active", "suspended", "deleted"]
}

table "app.users" {
  column "status" {
    type    = enum.app.status
    null    = false
    default = "pending"
  }
}
