variable "tenant" {
  type    = string
  default = "acme"
}

locals {
  tables = ["orders", "invoices"]
}

schema "app" {
  comment = "Tenant ${var.tenant}"
}

table "app.orders" {
  column "id" {
    type = integer
  }
}

table "app.invoices" {
  column "id" {
    type = integer
  }
  column "order_id" {
    type = integer
  }
  comment = format("%s invoices", upper(var.tenant))
}
