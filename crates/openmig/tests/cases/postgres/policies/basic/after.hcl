role "app_reader" {}

role "app_writer" {
  member_of = [role.app_reader]
}

schema "app" {}

table "app.notes" {
  column "id" {
    type = integer
  }
  column "owner" {
    type = text
    default = sql("current_user")
  }
  rls = true
  policy "notes_select" {
    for   = SELECT
    to    = [role.app_reader]
    using = "owner = current_user"
  }
  policy "notes_write" {
    for   = INSERT
    to    = [role.app_writer]
    check = "owner = current_user"
  }
}
