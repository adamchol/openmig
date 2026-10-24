role "app_reader" {}

role "app_writer" {
  member_of = [role.app_reader]
}

schema "app" {}

function "app.secret" {
  returns  = text
  language = sql
  body     = "SELECT 'x'"
}
