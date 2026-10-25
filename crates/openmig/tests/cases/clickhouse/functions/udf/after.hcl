function "add_one" {
  args = ["x"]
  body = "x + 2"
}

function "linear" {
  args = ["x", "k", "b"]
  body = "k * x + b"
}

database "app" {}

table "app.t" {
  engine = MergeTree
  column "x" {
    type = UInt64
  }
  column "y" {
    type    = UInt64
    default = sql("linear(x, 2, 1)")
  }
  order_by = [column.x]
}
