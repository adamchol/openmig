schema "app" {}

table "app.recordings" {
  column "id" {
    type = uuid
    null = false
  }
  column "title" {
    type = text
  }
  column "tags" {
    type = "uuid[]"
  }
  primary_key {
    columns = [column.id]
  }
  index "recordings_tags" {
    columns = [column.tags]
  }
}
