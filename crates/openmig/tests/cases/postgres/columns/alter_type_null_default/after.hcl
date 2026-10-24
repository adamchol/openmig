schema "app" {}

table "app.items" {
  column "id" {
    type = integer
    null = false
  }
  column "price" {
    type    = numeric(12, 2)
    null    = false
    default = 0
  }
  column "label" {
    type = text
  }
}
