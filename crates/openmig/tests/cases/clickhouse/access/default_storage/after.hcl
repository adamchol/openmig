database "app" {}

table "app.events" {
  engine = MergeTree
  column "tenant" {
    type = String
  }
  order_by = [column.tenant]
}

role "reader" {}

role "admin" {
  member_of = [role.reader]
}

user "loader" {
  auth  = no_password
  roles = [role.reader]
}

row_policy "tenant_a" {
  on    = table.app.events
  using = "tenant = 'a'"
  to    = [role.reader]
}
