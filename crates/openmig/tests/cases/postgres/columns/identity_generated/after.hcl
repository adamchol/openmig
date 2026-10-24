schema "app" {}

table "app.orders" {
  column "id" {
    type = bigint
    identity = {
      generated = ALWAYS
      start     = 1000
      increment = 10
    }
  }
  column "net" {
    type = numeric
    null = false
  }
  column "vat" {
    type = numeric
    null = false
  }
  column "gross" {
    type      = numeric
    generated = "net + vat"
  }
}
