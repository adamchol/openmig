database "shop" {}

table "shop.events" {
  engine = MergeTree
  column "timestamp" {
    type = DateTime
  }
  order_by = [column.timestamp]
}

role "reader" {
  storage = local_directory
}

user "old_svc" {
  auth    = no_password
  storage = local_directory
}
