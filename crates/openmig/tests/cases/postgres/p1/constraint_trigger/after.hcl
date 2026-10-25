schema "app" {}

function "app.check_balance" {
  returns  = trigger
  language = plpgsql
  body     = "BEGIN RETURN NULL; END;"
}

table "app.ledger" {
  column "amount" {
    type = numeric
  }
  trigger "ledger_balance" {
    constraint         = true
    deferrable         = true
    initially_deferred = true
    after              = [INSERT, UPDATE]
    for                = ROW
    execute            = function.app.check_balance
  }
}
