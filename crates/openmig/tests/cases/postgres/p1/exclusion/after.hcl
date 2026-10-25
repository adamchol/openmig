extension "btree_gist" {}

schema "app" {}

table "app.bookings" {
  column "room" {
    type = integer
    null = false
  }
  column "during" {
    type = tstzrange
    null = false
  }
  exclude "bookings_no_overlap" {
    using = gist
    key {
      column = column.room
      operator = "="
    }
    key {
      column = column.during
      operator = "&&"
    }
  }
}
