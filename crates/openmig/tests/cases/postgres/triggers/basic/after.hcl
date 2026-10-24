schema "app" {}

function "app.touch" {
  returns  = trigger
  language = plpgsql
  body     = "BEGIN NEW.updated_at := now(); RETURN NEW; END;"
}

table "app.posts" {
  column "id" {
    type = integer
  }
  column "title" {
    type = text
  }
  column "updated_at" {
    type = timestamptz
  }
  trigger "posts_touch" {
    before    = [UPDATE]
    update_of = [column.title]
    for       = ROW
    when      = "OLD.title IS DISTINCT FROM NEW.title"
    execute   = function.app.touch
  }
}
