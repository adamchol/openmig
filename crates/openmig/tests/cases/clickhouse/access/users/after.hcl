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

user "app_svc" {
  auth             = sha256_hash("2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b")
  storage          = local_directory
  hosts            = ["10.0.0.0/8"]
  roles            = [role.reader]
  default_roles    = [role.reader]
  default_database = database.shop
}

grant {
  privileges = [INSERT]
  on         = [table.shop.events]
  to         = [user.app_svc]
}
