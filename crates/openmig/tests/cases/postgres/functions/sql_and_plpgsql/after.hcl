schema "app" {}

function "app.add" {
  arg "a" {
    type = integer
  }
  arg "b" {
    type    = integer
    default = 1
  }
  returns    = integer
  language   = sql
  volatility = IMMUTABLE
  strict     = true
  body       = "SELECT a + b"
}

function "app.current_tenant" {
  returns    = integer
  language   = plpgsql
  volatility = STABLE
  security   = DEFINER
  set        = { search_path = "pg_catalog, public" }
  body       = <<-SQL
    BEGIN
      RETURN nullif(current_setting('app.tenant', true), '')::integer;
    END;
  SQL
}

procedure "app.noop" {
  arg "msg" {
    type = text
  }
  language = plpgsql
  body     = "BEGIN RAISE NOTICE '%', msg; END;"
}
