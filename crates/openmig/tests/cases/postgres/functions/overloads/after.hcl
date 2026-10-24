schema "app" {}

function "app.fmt_int" {
  name = "fmt"
  arg "v" {
    type = integer
  }
  returns  = text
  language = sql
  body     = "SELECT v::text"
}

function "app.fmt_bool" {
  name = "fmt"
  arg "v" {
    type = boolean
  }
  returns  = text
  language = sql
  body     = "SELECT CASE WHEN v THEN 'yes' ELSE 'no' END"
}
