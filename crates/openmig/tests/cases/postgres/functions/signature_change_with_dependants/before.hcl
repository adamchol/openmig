schema "app" {}

function "app.default_score" {
  arg "base" {
    type = integer
  }
  returns  = integer
  language = sql
  body     = "SELECT base * 2"
}

table "app.players" {
  column "score" {
    type    = integer
    default = sql("app.default_score(10)")
  }
}
