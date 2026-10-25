database "app" {}

table "app.events" {
  engine = MergeTree
  column "id" {
    type = UInt64
  }
  column "secret" {
    type = String
  }
  order_by = [column.id]
}

role "web" {
  storage = local_directory
}

grant {
  privileges = [ALL]
  on         = [database.app]
  to         = [role.web]
}

grant {
  privileges = [SELECT]
  on         = ["*.*"]
  to         = [role.web]
  with_grant_option = true
}
