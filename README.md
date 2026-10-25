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

## Documentation

* [`docs/guide.md`](docs/guide.md): getting started, the development loop, CI, deployment,
  adopting OpenMig on an existing database.
* [`docs/spec/hcl.md`](docs/spec/hcl.md): the schema language for PostgreSQL and ClickHouse.
* [`docs/spec/config.md`](docs/spec/config.md), [`docs/spec/cli.md`](docs/spec/cli.md),
  [`docs/spec/formats.md`](docs/spec/formats.md): configuration, commands, migration files and the
  history table.
* [`docs/spec/architecture.md`](docs/spec/architecture.md): crates, normalization, planning rules.
* [`deploy/`](deploy): container images, Kubernetes Job, Helm chart.

## Status

* **PostgreSQL 14–18**: schemas, tables (identity, generated, partitioned, unlogged, storage),
  constraints (primary, foreign, unique, check, exclusion), indexes, views, materialized views,
  functions, procedures, triggers, event triggers, row-level security policies, enums, composites,
  domains, sequences, extensions, roles and memberships, grants, comments, table data, renames.
  A real-world history of 173 migrations replays, round-trips through `inspect` and converges
  migration by migration.
* **ClickHouse 24.8+** (tested on 26.5): databases (Atomic, Replicated), MergeTree-family and other
  engines, codecs, defaults, TTLs, settings, skip indexes, projections, constraints, views,
  materialized views (`TO`, inner engine, refreshable), dictionaries, SQL functions, roles,
  grants (collapsed by the server's privilege hierarchy), row policies, `ON CLUSTER`, renames.

## Development

```console
$ nix develop                # Rust, PostgreSQL 17, ClickHouse
$ cargo test --workspace     # unit, integration, golden and end-to-end tests (starts servers)
$ nix flake check            # the same in the Nix sandbox, plus fmt and clippy
```

License: Apache-2.0.
