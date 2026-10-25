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

role "auditor" {
  storage = replicated
}

role "admin" {
  storage   = local_directory
  member_of = [{ role = role.reader, admin = true }, role.auditor]
}

row_policy "tenant_a" {
  on    = table.app.events
  using = "tenant IN ('a', 'b')"
  as    = RESTRICTIVE
  to    = [ALL]
  except = [role.admin]
}
