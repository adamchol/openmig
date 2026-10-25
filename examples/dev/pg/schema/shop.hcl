schema "shop" {}

enum "shop.order_status" {
  values = ["pending", "paid", "shipped"]
}

table "shop.customers" {
  column "id" {
    type     = bigint
    identity = BY_DEFAULT
  }
  column "email" {
    type = text
    null = false
  }
  column "created_at" {
    type    = timestamptz
    null    = false
    default = sql("now()")
  }
  primary_key {
    columns = [column.id]
  }
  index "customers_email_key" {
    unique = true
    key {
      expr = "lower(email)"
    }
  }
}

table "shop.orders" {
  column "id" {
    type     = bigint
    identity = BY_DEFAULT
  }
  column "customer_id" {
    type = bigint
    null = false
  }
  column "status" {
    type    = enum.shop.order_status
    null    = false
    default = "pending"
  }
  column "total" {
    type = numeric(12, 2)
    null = false
  }
  primary_key {
    columns = [column.id]
  }
  foreign_key "orders_customer_fk" {
    columns    = [column.customer_id]
    references = [table.shop.customers.column.id]
    on_delete  = CASCADE
  }
  check "orders_total_positive" {
    expr = "total >= 0"
  }
}

view "shop.open_orders" {
  as = "SELECT id, customer_id, total FROM shop.orders WHERE status <> 'shipped'"
}
