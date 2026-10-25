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
  privileges = [SELECT, INSERT, "ALTER UPDATE", dictGet]
  on         = [database.app]
  to         = [role.web]
}

grant {
  privileges = [SELECT]
  on         = ["*.*"]
  to         = [role.web]
}

grant {
  privileges = [SELECT]
  on         = [table.app.events]
  columns    = [table.app.events.column.id]
  to         = ["web"]
  with_grant_option = true
}
