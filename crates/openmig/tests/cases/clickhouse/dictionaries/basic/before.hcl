database "app" {}

table "app.countries" {
  engine = MergeTree
  column "id" {
    type = UInt64
  }
  column "name" {
    type = String
  }
  order_by = [column.id]
}

dictionary "app.country_names" {
  attribute "id" {
    type = UInt64
  }
  attribute "name" {
    type    = String
    default = "unknown"
  }
  primary_key = ["id"]
  source      = "CLICKHOUSE(TABLE 'countries' DB 'app')"
  layout      = HASHED
  lifetime    = 300
}
