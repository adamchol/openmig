schema "app" {}

table "app.accounts" {
  column "id" {
    type = integer
    null = false
  }
  column "email" {
    type = text
  }
  column "balance" {
    type = numeric
    null = false
  }
  check "accounts_balance_positive" {
    expr = "balance > -100"
  }
}
