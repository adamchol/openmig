schema "app" {}

function "app.answer" {
  returns  = integer
  language = sql
  body     = "SELECT 42"
}
