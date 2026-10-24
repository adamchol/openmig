schema "app" {}

table "app.orgs" {
  column "id" {
    type = integer
    null = false
  }
  primary_key {
    columns = [column.id]
  }
}

table "app.members" {
  column "org_id" {
    type = integer
    null = false
  }
  column "user_id" {
    type = integer
    null = false
  }
  primary_key {
    columns = [column.org_id, column.user_id]
  }
  foreign_key "members_org_fk" {
    columns    = [column.org_id]
    references = [table.app.orgs.column.id]
    on_delete  = CASCADE
    deferrable = true
    initially_deferred = true
  }
}
