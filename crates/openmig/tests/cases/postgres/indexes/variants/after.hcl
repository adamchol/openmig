schema "app" {}

table "app.users" {
  column "id" {
    type = integer
    null = false
  }
  column "email" {
    type = text
    null = false
  }
  column "tenant_id" {
    type = integer
  }
  column "created_at" {
    type = timestamptz
  }
  column "profile" {
    type = jsonb
  }
  index "users_lower_email_key" {
    unique = true
    key {
      expr = "lower(email)"
    }
  }
  index "users_recent_idx" {
    key {
      column = column.tenant_id
    }
    key {
      column = column.created_at
      desc   = true
      nulls  = LAST
    }
    where = "created_at IS NOT NULL"
  }
  index "users_profile_idx" {
    using = gin
    key {
      column  = column.profile
      opclass = jsonb_path_ops
    }
  }
  index "users_tenant_cover_idx" {
    columns = [column.tenant_id]
    include = [column.email]
  }
  index "users_id_hash_idx" {
    using   = hash
    columns = [column.id]
  }
}
