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
  privileges = [USAGE]
  on         = [schema.app]
  to         = [role.app_reader, role.app_writer]
}

grant {
  privileges = [SELECT]
  on         = [table.app.users, table.app.orgs]
  to         = [role.app_reader]
}

grant {
  privileges = [UPDATE]
  on         = [table.app.users]
  columns    = ["email"]
  to         = [role.app_writer]
  with_grant_option = true
}
