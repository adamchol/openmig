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
  unique "accounts_email_key" {
    columns        = [column.email]
    nulls_distinct = false
  }
  check "accounts_balance_positive" {
    expr = "balance >= 0"
  }
}
