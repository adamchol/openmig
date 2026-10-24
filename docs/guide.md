# OpenMig guide

OpenMig keeps a database schema as code. You declare the schema in HCL; OpenMig works out the SQL.

There are two ways to use it, usually together:

* **Develop**: `openmig sync` makes your local database match the schema files. With `--watch` it
  re-runs whenever you save.
* **Ship**: `openmig generate` writes the difference between the committed migrations and the schema
  as a new SQL file. The file is reviewed and committed; `openmig deploy` applies pending files in
  production; `openmig check` makes sure nobody forgot to generate one.

## Installing

With Nix: `nix run github:dialohq/openmig -- --help`, or add the flake's `packages.openmig` to your
dev shell. From source: `cargo install --path crates/openmig`.

OpenMig needs the database server binaries for the scratch database it uses to normalize schemas
(see below): `initdb` and `postgres` for PostgreSQL, `clickhouse` for ClickHouse. In a Nix dev shell
add `pkgs.postgresql_17` (the same major version as production) and/or `pkgs.clickhouse`.

## A first schema

```
db/
  openmig.toml
  schema/
    app.hcl
  migrations/
```

```toml
# db/openmig.toml
[databases.app]
engine = "postgres"
schema = ["schema"]
migrations = "migrations"
url = "${DATABASE_URL:-postgresql://postgres@localhost/app}"
version = 17
```

```hcl
# db/schema/app.hcl
schema "app" {}

enum "app.plan" {
  values = ["free", "pro"]
}

table "app.accounts" {
  column "id" {
    type     = bigint
    identity = BY_DEFAULT
  }
  column "email" {
    type = text
    null = false
  }
  column "plan" {
    type    = enum.app.plan
    null    = false
    default = "free"
  }
  column "created_at" {
    type    = timestamptz
    null    = false
    default = sql("now()")
  }
  primary_key {
    columns = [column.id]
  }
  index "accounts_email_key" {
    unique = true
    key {
      expr = "lower(email)"
    }
  }
}
```

The full language is in [`spec/hcl.md`](spec/hcl.md).

## Developing: `sync`

```console
$ openmig sync
-- app: 4 statement(s)
-- Create schema "app"
CREATE SCHEMA "app";
...
Apply 4 statement(s)? [y/N]
```

* `--watch` keeps running and re-syncs on every change to the schema files.
* `--yes` applies without asking (needed when not in a terminal, e.g. under process-compose).
  Destructive statements (dropping tables or columns, deleting rows) still need `--allow-destructive`
  or an interactive confirmation.
* `--dry-run` prints the plan only.

### The scratch database

To compare your HCL with a real database, OpenMig first lets the database itself normalize the
schema: it starts a private, throwaway server, applies the schema there, and reads it back. You
never configure this "dev database": OpenMig starts it from the binaries on your `PATH` (or
`[databases.<name>.scratch] bin_dir`), on a unix socket in `/tmp`, and stops it when the command
ends. In watch mode the server stays up between runs. Starting one takes about 0.1 s for PostgreSQL
(the initialized data directory is cached) and under a second for ClickHouse.

Use the same major version as your production server (`version = 17` makes OpenMig check it).

### Test-only objects

Add extra schema directories for one command with `--schema`:

```console
$ openmig sync --schema tests/schema
```

## Shipping: migrations

```console
$ openmig generate add_accounts
db/migrations/20261024120000_add_accounts.sql
```

The file contains plain SQL, one statement per change, each preceded by a comment. It is also
recorded in `db/migrations/openmig.sum`, a checksum file that makes two branches that both add a
migration conflict in git, so the second one regenerates.

* Edit generated files only if you must; `openmig rehash` updates the checksums afterwards.
* For data backfills or anything the planner cannot express, write SQL by hand:
  `openmig new backfill_plans` creates an empty file. `generate` replays hand-written files like any
  other, so later generated migrations take them into account.
* Statements that cannot run in a transaction (`CREATE INDEX CONCURRENTLY` with
  `concurrent_indexes = true`) put `-- openmig:no-transaction` at the top of the file.

### In CI

`check` fails (exit code 4) when the schema has changes that no migration contains, and verifies the
checksum file and that every migration replays cleanly:

```console
$ openmig check --all --base origin/main
```

To let a bot write the migration for each pull request, run `generate` with `--base`: it first
deletes generated migrations that are not on the base branch yet, so a branch always carries exactly
one generated migration, however often the schema changes. Hand-written migrations are kept. See
[`examples/github-actions.yml`](examples/github-actions.yml).

### Deploying

```console
$ openmig deploy --url "$PRODUCTION_URL"
```

`deploy` takes a lock (a PostgreSQL advisory lock; a Keeper-backed lock on ClickHouse), applies every
pending file in order and records it in the `openmig.history` table. PostgreSQL files run in one
transaction each and are retried when they time out waiting for a lock (`[databases.<name>.deploy]
lock_timeout`, `retries`). Files without a transaction (and all ClickHouse files) record progress per
statement; a failed file resumes where it stopped once fixed.

`openmig status` lists applied and pending migrations; `status --drift` also compares the live
database with what the migrations produce.

#### Kubernetes

Bake the migration directory into an image and run `openmig deploy` as a Job. No config file is
needed in the image:

```nix
# flake.nix of your application
migrationsImage = openmig.lib.mkMigrationsImage {
  inherit pkgs;
  name = "ghcr.io/acme/app-migrations";
  tag = self.shortRev or "dirty";
  engine = "postgres";
  migrations = ./db/migrations;
};
```

or with Docker: [`deploy/docker/Dockerfile`](../deploy/docker/Dockerfile).

The image runs `openmig deploy --engine postgres --migrations /migrations --wait-for-db 2m` against
`$OPENMIG_URL`. Run it as an Argo CD sync hook before the application
([`deploy/kubernetes/job.yaml`](../deploy/kubernetes/job.yaml)) or with the Helm chart in
[`deploy/helm/openmig-migrations`](../deploy/helm/openmig-migrations). Re-running it is cheap: with
nothing pending it exits immediately.

## Several databases

One `openmig.toml` can describe several databases (e.g. PostgreSQL and ClickHouse). Pick one with
`--db NAME` or run a command for all with `--all`.

## Renames

Renaming a column or table in HCL looks like a drop and a create. Tell OpenMig what happened:

```hcl
column "full_name" {
  type         = text
  renamed_from = "name"
}
```

The hint only applies while the old name still exists, so it can stay in the schema until it is
convenient to remove. In an interactive `sync` or `generate`, OpenMig also asks when a drop and a
create look like a rename, and prints the `renamed_from` line to add so CI does the same.

## Coming from Atlas

* Translate the HCL (the languages are close; see [`spec/hcl.md`](spec/hcl.md)): `table "x" "y"` with
  `schema = schema.x` becomes `table "x.y"`; `permission` blocks become `grant`/`revoke` blocks (and
  default `PUBLIC` privileges no longer need declaring); `atlas.hcl` envs become one
  `[databases.<name>]` table plus environment variables. `openmig inspect --split-by schema` on a
  database built by Atlas produces a complete starting point.
* Start a fresh migration directory: `openmig generate baseline` writes one file for the whole schema.
* On each existing database, record that baseline as applied without running it:
  `openmig deploy --baseline <version-of-the-baseline-file>`.
