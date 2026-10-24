database "app" {}

table "app.visits" {
  renamed_from = "app.hits"
  engine = MergeTree
  column "ts" {
    type = DateTime
  }
  column "page" {
    type         = String
    renamed_from = "url"
  }
  order_by = [column.ts]
}
