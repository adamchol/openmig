# `openmig.toml`

OpenMig looks for `openmig.toml` in the current directory and its parents (or `--config PATH`, or
`OPENMIG_CONFIG`). Relative paths in the file are relative to the file's directory.

```toml
[databases.app]
engine = "postgres"                 # postgres | clickhouse (required)
schema = ["db/pg/schema"]           # schema files or directories (recursive)
migrations = "db/pg/migrations"     # migration directory
url = "${DATABASE_URL}"             # target for sync/deploy/status/inspect; --url overrides
version = 17                        # server major version for the scratch server

# Optional, common to both engines
schemas = "*"                       # "*" (default) = every non-system schema; or ["app", "public"]
exclude = ["role:cnpg_*"]           # [kind:]glob patterns; see below
manage_roles = false                # true: own every role on the server (see Scope)
data = "sync"                       # off | insert | sync: default mode for `data` blocks
data_max_rows = 10000               # refuse to manage rows of bigger tables
history_table = "openmig.history"

[databases.app.vars]                # values for schema `variable`s
tenant = "acme"

[databases.app.deploy]
lock_timeout = "5s"                 # per transactional file (PostgreSQL)
retries = 5                         # retries after a lock timeout
lock_wait = "10m"                   # wait for the migration lock
allow_out_of_order = false

# PostgreSQL only
default_schema = "public"
search_path = ["$user", "public"]   # session search_path for applying SQL
public_defaults = "keep"            # keep | revoke: start from PostgreSQL's PUBLIC defaults or none
concurrent_indexes = false          # CREATE/DROP INDEX CONCURRENTLY on existing tables

[databases.app.scratch]             # PostgreSQL scratch server
bin_dir = "/nix/store/...-postgresql-17/bin"  # default: PATH
run_as = "nobody"                   # when running as root
initdb_args = ["--encoding=UTF8", "--locale=C.UTF-8"]
settings = { shared_preload_libraries = "pg_stat_statements" }

[databases.analytics]
engine = "clickhouse"
schema = ["db/ch/schema"]
migrations = "db/ch/migrations"
url = "${CLICKHOUSE_URL}"           # http(s)://user:password@host:8123[/database]

# ClickHouse only
cluster = "{cluster}"               # enable ON CLUSTER; name or macro

[databases.analytics.scratch]       # ClickHouse scratch server
bin = "clickhouse"
storage_policies = ["encrypted"]
profile_settings = { allow_experimental_time_series_table = "1" }
macros = { shard = "01", replica = "r1" }
extra_config = "db/ch/scratch.xml"
```

## Environment variables

String values may contain `${VAR}`, `${VAR:-default}` and `$$` (a literal `$`). Expansion is lazy:
an unset variable is an error only for a command that needs that value, so `url = "${DATABASE_URL}"`
does not stop `openmig generate` from working in CI without a database.

## Excludes

`[kind:]glob` matched against the object's dotted path (`app.users`, `app.users.email`, `web`). `*`
does not cross dots, `**` does. Without a kind, excluding a schema or table also excludes everything
inside it. Kinds: `schema`, `database`, `table`, `column`, `index`, `view`, `materialized_view`,
`function`, `procedure`, `trigger`, `policy`, `enum`, `composite`, `domain`, `sequence`, `extension`,
`role`, `user`, `grant`, `dictionary`, ...

Always excluded: system schemas/databases (`pg_*`, `information_schema`, `system`,
`INFORMATION_SCHEMA`), the history schema/database, built-in roles (`pg_*`), the bootstrap
superuser and the connecting user.

## Scope

OpenMig can live inside a server it does not own. With `schemas = ["app"]` it inspects, plans and
checks for drift only the listed schemas (PostgreSQL) or databases (ClickHouse) and what is in
them. Declaring a schema or database outside the list (or an object in one) is an error; the
implicit `public`/`default` namespace is simply left alone.

Objects that live in no schema are compared only when OpenMig owns them, that is when the
reference side of the comparison has them: the schema for `sync` and `check`, the migrations for
`status --drift`. Everything else on the server is never planned, dropped or reported as drift.

| Objects | Owned |
|---|---|
| Roles (both engines) | Declared ones (and those created by earlier migrations, so `generate` drops a role the schema stops declaring). With `manage_roles = true`: every role. |
| ClickHouse users | Declared ones, even with `manage_roles = true`. |
| Event triggers, publications (PostgreSQL), SQL functions (ClickHouse) | Declared ones when `schemas` is a list; every one with `schemas = "*"`. |

Privileges and role memberships are compared only for owned roles and users (and `PUBLIC`), so
grants that the server's administrators give to their own roles, including through
`ALTER DEFAULT PRIVILEGES`, are never revoked. A declared PostgreSQL role that already exists is
adopted: its migration creates it when missing and otherwise sets its declared attributes.

## Config-less mode

`deploy` and `status` work without a config file, for container images:

```
openmig deploy --engine postgres --migrations /migrations --url "$DATABASE_URL"
```
