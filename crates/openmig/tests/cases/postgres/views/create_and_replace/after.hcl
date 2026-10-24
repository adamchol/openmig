schema "app" {}

table "app.users" {
  column "id" {
    type = integer
    null = false
  }
  column "email" {
    type = text
  }
  column "active" {
    type = boolean
    null = false
    default = true
  }
}

view "app.active_users" {
  as = "SELECT id, email FROM app.users WHERE active"
  security_invoker = true
}

view "app.active_user_count" {
  as = "SELECT count(*) AS n FROM app.active_users"
}
