schema "app" {}

table "app.recordings" {
  column "id" {
    type = serial
    null = false
  }
  column "title" {
    type = text
  }
  column "tags" {
    type = "integer[]"
  }
  primary_key {
    columns = [column.id]
  }
  index "recordings_tags" {
    columns = [column.tags]
  }
}
