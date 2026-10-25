database "shop" {}

table "shop.events" {
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
  on         = [database.shop]
  to         = [role.reader]
}

grant {
  privileges = [ALL]
  on         = [table.shop.events]
  to         = [role.gateway]
  with_grant_option = true
}

grant {
  privileges = [SELECT]
  on         = [table.shop.events]
  columns    = ["call_id"]
  to         = [role.gateway]
}
