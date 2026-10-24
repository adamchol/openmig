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
manage_roles = false                # manage cluster-wide roles/users and memberships
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

## Config-less mode

`deploy` and `status` work without a config file, for container images:

```
openmig deploy --engine postgres --migrations /migrations --url "$DATABASE_URL"
```
