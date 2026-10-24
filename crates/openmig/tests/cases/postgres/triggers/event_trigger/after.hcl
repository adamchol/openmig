schema "app" {}

function "app.on_ddl" {
  returns  = event_trigger
  language = plpgsql
  body     = "BEGIN RAISE NOTICE 'ddl'; END;"
}

event_trigger "notify_ddl" {
  on      = ddl_command_end
  tags    = ["CREATE TABLE", "ALTER TABLE"]
  execute = function.app.on_ddl
}
