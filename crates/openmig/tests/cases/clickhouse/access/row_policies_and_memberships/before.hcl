database "app" {}

table "app.events" {
  engine = MergeTree
  column "tenant" {
    type = String
  }
  order_by = [column.tenant]
}

role "reader" {
  storage = local_directory
}

role "admin" {
  storage   = local_directory
  member_of = [role.reader]
}

row_policy "tenant_a" {
  on    = table.app.events
  using = "tenant = 'a'"
  to    = [role.reader]
}
