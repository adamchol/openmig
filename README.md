# OpenMig

Declarative and versioned schema migrations for PostgreSQL and ClickHouse.

* Declare the schema in HCL: tables, indexes, constraints, functions, triggers, row-level security,
  views, types, roles and grants (PostgreSQL); engines, ordering keys, TTLs, codecs, skip indexes,
  materialized views, roles and grants (ClickHouse).
* **Develop** with `openmig sync --watch`: your local database follows the schema as you edit it.
* **Ship** with versioned SQL: `openmig generate` writes a migration file (typically in CI),
  `openmig check` guards pull requests, `openmig deploy` applies migrations in production — for
  example as a Kubernetes Job from an image that contains the migration files.
* No "dev database" to set up: OpenMig starts a private scratch server from the `postgres` or
  `clickhouse` binaries on your `PATH` whenever it needs to normalize a schema.

```toml
# openmig.toml
[databases.app]
engine = "postgres"
schema = ["db/schema"]
migrations = "db/migrations"
url = "${DATABASE_URL}"
version = 17
```

```hcl
# db/schema/app.hcl
schema "app" {}

table "app.users" {
  column "id" {
    type     = bigint
    identity = BY_DEFAULT
  }
  column "email" {
    type = text
    null = false
  }
  primary_key {
    columns = [column.id]
  }
  index "users_email_key" {
    columns = [column.email]
    unique  = true
  }
}
```

```console
$ openmig sync --watch          # local development
$ openmig generate add_users    # write db/migrations/<timestamp>_add_users.sql
$ openmig check                 # CI: migrations are valid and match the schema
$ openmig deploy --url "$PROD"  # apply pending migrations
```

Documentation: [`docs/spec`](docs/spec).

License: Apache-2.0.
