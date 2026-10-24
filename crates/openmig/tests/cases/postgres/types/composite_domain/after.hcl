schema "app" {}

composite "app.money" {
  field "amount" {
    type = numeric(12, 2)
  }
  field "currency" {
    type = char(3)
  }
}

domain "app.positive_money" {
  type = composite.app.money
  check "positive" {
    expr = "(VALUE).amount > 0"
  }
}

domain "app.email" {
  type = text
  null = false
  check "has_at" {
    expr = "VALUE ~ '@'"
  }
}

table "app.payments" {
  column "value" {
    type = domain.app.positive_money
  }
  column "payer" {
    type = domain.app.email
  }
}
