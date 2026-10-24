database "app" {}

table "app.hits" {
  engine = MergeTree
  column "ts" {
    type = DateTime
  }
  column "url" {
    type = String
  }
  order_by = [column.ts]
}
