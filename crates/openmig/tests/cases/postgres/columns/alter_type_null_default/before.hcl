schema "app" {}

table "app.items" {
  column "id" {
    type = integer
    null = false
  }
  column "price" {
    type = numeric(10, 2)
  }
  column "label" {
    type    = varchar(50)
    null    = false
    default = "none"
  }
}
