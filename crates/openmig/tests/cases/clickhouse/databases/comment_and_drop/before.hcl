database "keep" {
  comment = "old"
}

database "gone" {}

table "gone.t" {
  engine = MergeTree
  column "id" {
    type = UInt64
  }
  order_by = [column.id]
}
