schema "app" {}

table "app.tickets" {
  column "id" {
    type = bigserial
  }
  column "number" {
    type    = integer
    default = sql("nextval('app.ticket_numbers')")
  }
}

sequence "app.ticket_numbers" {
  type      = integer
  start     = 100
  increment = 5
  owned_by  = table.app.tickets.column.number
}
