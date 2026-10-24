extension "citext" {
  schema = schema.public
}

schema "app" {}

table "app.users" {
  column "email" {
    type = "public.citext"
  }
}
