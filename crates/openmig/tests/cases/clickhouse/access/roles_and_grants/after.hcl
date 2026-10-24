database "dialo" {}

table "dialo.events" {
  engine = MergeTree
  column "timestamp" {
    type = DateTime
  }
  column "call_id" {
    type = String
  }
  order_by = [column.timestamp]
}

role "reader" {
  storage = local_directory
}

role "gateway" {
  storage = local_directory
}

grant {
  privileges = [SELECT]
  on         = [database.dialo]
  to         = [role.reader]
}

grant {
  privileges = [ALL]
  on         = [table.dialo.events]
  to         = [role.gateway]
  with_grant_option = true
}

grant {
  privileges = [SELECT]
  on         = [table.dialo.events]
  columns    = ["call_id"]
  to         = [role.gateway]
}
