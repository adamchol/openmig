database "analytics" {}

table "analytics.types" {
  engine = MergeTree
  column "id" {
    type = UUID
  }
  column "flag" {
    type = Bool
  }
  column "status" {
    type = "Enum8('new' = 1, 'done' = 2)"
  }
  column "tags" {
    type = Array(String)
  }
  column "words" {
    type = "Array(Tuple(word String, count UInt32))"
  }
  column "price" {
    type = Decimal(18, 4)
  }
  column "hash" {
    type = FixedString(32)
  }
  column "maybe" {
    type = Nullable(String)
  }
  column "alias_len" {
    type  = UInt64
    alias = "length(hash)"
  }
  order_by = [column.id]
}
