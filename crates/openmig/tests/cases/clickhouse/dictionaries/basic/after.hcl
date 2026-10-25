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
    default = "?"
  }
  primary_key = [attribute.id]
  source      = "CLICKHOUSE(TABLE 'countries' DB 'app')"
  layout      = FLAT()
  lifetime    = "MIN 60 MAX 600"
  comment     = "Country names by id"
}
