role "app_reader" {}

role "app_writer" {
  member_of = [role.app_reader]
}

schema "app" {}

table "app.users" {
  column "id" {
    type = integer
  }
  column "email" {
    type = text
  }
}

table "app.orgs" {
  column "id" {
    type = integer
  }
}

grant {
  privileges = [ALL]
  on         = [table.app.orgs]
  to         = [role.app_writer]
}
