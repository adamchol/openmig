schema "app" {}

function "app.default_score" {
  arg "base" {
    type = bigint
  }
  returns  = bigint
  language = sql
  body     = "SELECT base * 2"
}

table "app.players" {
  column "score" {
    type    = bigint
    default = sql("app.default_score(10)")
  }
}
