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

role "legacy" {
  storage = local_directory
}

grant {
  privileges = [INSERT]
  on         = [database.dialo]
  to         = [role.reader]
}
