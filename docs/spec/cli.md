# Command line

```
openmig [GLOBAL FLAGS] <COMMAND> [FLAGS]
```

## Global flags

| Flag | Env | Meaning |
|---|---|---|
| `--config PATH` | `OPENMIG_CONFIG` | Config file (default: search upward for `openmig.toml`). |
| `-d, --db NAME` | `OPENMIG_DB` | Database from the config. Optional when the config has one. |
| `--all` | | Run for every database in the config. |
| `--url URL` | `OPENMIG_URL` | Override the database URL. |
| `--schema PATH` | | Additional schema file/directory (repeatable), e.g. test-only objects. |
| `--var NAME=VALUE` | | Schema variable (repeatable). |
| `--engine postgres\|clickhouse` | `OPENMIG_ENGINE` | Config-less mode (with `--migrations`); used only when no `openmig.toml` is found. |
| `--migrations DIR` | | Config-less mode, or override the migration directory. |
| `--log-format text\|json` | `OPENMIG_LOG_FORMAT` | Log format. |
| `--json` | | Machine-readable command output. |
| `-v, --verbose` / `-q, --quiet` | | Log level. `RUST_LOG` also works. |
| `--color auto\|always\|never` | | |

## Commands

| Command | Purpose |
|---|---|
| `sync` | Make a database match the schema (local development). |
| `generate [NAME]` | Write a new migration file for the difference between the migrations and the schema. |
| `check` | CI gate: the migration directory is valid and matches the schema. |
| `deploy` | Apply pending migration files. |
| `status` | Show applied and pending migrations. |
| `new NAME` | Create an empty migration file for hand-written SQL. |
| `rehash` | Rewrite `openmig.sum`. |
| `inspect` | Print a database as HCL (or SQL). |
| `diff --from SRC --to SRC` | Print the SQL that turns one state into another. |
| `history list\|mark-applied V\|delete V` | Repair the history table (P1). |

### `sync`

`--watch` (re-run when schema files or the config change; the scratch server stays up),
`-y, --yes` (no confirmation for non-destructive plans), `--allow-destructive` (apply destructive
statements without asking), `--dry-run` (print the plan only), `--recreate` (allow drop + create for
changes that cannot be altered in place), `--no-data` (ignore `data` blocks).

Destructive plans need interactive confirmation or `--allow-destructive`; otherwise exit 3.
After applying, the database is inspected again and must match the schema.

### `generate [NAME]`

`--base GIT_REF` (first delete generated migration files that do not exist in `GIT_REF`, so a CI bot
always leaves exactly one generated file per branch; hand-written files are kept), `--dry-run`.
Prints the new file's path, or "no changes". The new file is verified by replaying every migration
into a fresh scratch database and comparing with the schema.

### `check`

`--base GIT_REF` (files that exist in `GIT_REF` are unchanged and new files are newer than the
newest one there). Exit 4 when the schema has changes that no migration contains.

### `deploy`

`--to VERSION`, `--baseline VERSION`, `--allow-dirty`, `--allow-out-of-order`, `--dry-run`,
`--wait-for-db DURATION` (keep retrying the connection, for Kubernetes jobs), `--lock-wait DURATION`.

### `status`

`--drift` (also compare the live database with the replayed migrations), `--fail-on-pending`
(exit 4 when files are pending).

### `inspect`

`--format hcl|sql`, `--split-by none|schema|kind`, `-o, --output DIR` (write files instead of
printing).

### `diff`

`--from` / `--to` take `schema` (the configured schema), `migrations` (the replayed migration
directory), `url:<URL>` or a path to HCL files.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success, or nothing to do |
| 1 | Error |
| 2 | Configuration or schema error |
| 3 | Destructive change refused |
| 4 | Drift or pending migrations found (`check`, `status --drift`, `status --fail-on-pending`) |
| 5 | Timed out waiting for the migration lock |
| 6 | Database unreachable |
| 7 | Unsupported change |
