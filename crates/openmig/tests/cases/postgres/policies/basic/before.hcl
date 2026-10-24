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
    using = "true"
  }
}
