schema "app" {}

function "app.audit" {
  returns  = trigger
  language = plpgsql
  body     = "BEGIN RETURN NULL; END;"
}

table "app.a" {
  column "id" {
    type = integer
  }
}

table "app.b" {
  column "id" {
    type = integer
  }
}

trigger "audit" {
  for_each = [table.app.a, table.app.b]
  on       = each.value
  after    = [INSERT, UPDATE, DELETE]
  for      = STATEMENT
  execute  = function.app.audit
}
