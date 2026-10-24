# File formats and the history table

## Migration directory

```
migrations/
  20260216163954_baseline.sql
  20260217163957.sql
  20261024120000_add_users.sql
  openmig.sum
```

* File names: `<version>[_<name>].sql`. `version` is 14 digits, a UTC timestamp `YYYYMMDDHHMMSS`.
  `name` matches `[a-z0-9_]+`. Other names ending in `.sql` are errors; non-`.sql` files are ignored.
  Versions must be unique. Files are ordered by version.
* `openmig new` and `openmig generate` take the current UTC time as the version, bumped one second
  past the newest existing version when the clock is behind.

### Migration file

```sql
-- openmig:generated
-- openmig:no-transaction

-- Create index "users_email_idx" on "app"."users"
DROP INDEX CONCURRENTLY IF EXISTS "app"."users_email_idx";
-- Create index "users_email_idx" on "app"."users"
CREATE INDEX CONCURRENTLY "users_email_idx" ON "app"."users" ("email");
```

* **Directives** live in the leading comment block (the `--` lines and blank lines before the first
  statement):
  * `-- openmig:generated`: written by `generate`. `generate --base <ref>` may delete and regenerate
    generated files that are not in `<ref>`. Hand-written files never carry it.
  * `-- openmig:no-transaction`: statements run one by one outside a transaction.
  * `-- openmig:lock-timeout <duration>`, `-- openmig:statement-timeout <duration>` (`30s`, `5m`).
    A duration is a whole number followed by `ms`, `s`, `m` or `h`.
  * Any other `-- openmig:` line is an error, and so is a repeated directive. Other comments (such
    as Atlas's `-- atlas:txmode none`) are ignored.
* Statements are separated by `;` at the top level (outside strings, quoted identifiers, comments,
  PostgreSQL dollar-quoted bodies and `BEGIN ATOMIC ... END` bodies). Like psql, PostgreSQL files
  also keep `;` inside parentheses (`CREATE RULE ... DO (...; ...)`).
* The `--` lines directly above a statement (no blank line in between) are its comment; the
  checksum of a statement covers its text only.
* Generated files quote and fully qualify every identifier. Each statement is preceded by a
  one-line `-- <comment>`.
* Generated files that create SQL-language functions begin with `SET check_function_bodies = off;`
  so function bodies may reference objects created later in the file.
* PostgreSQL files run in one transaction per file unless `no-transaction`. ClickHouse has no
  transactions: every file runs statement by statement and progress is recorded per statement.

## `openmig.sum`

```
openmig-sum 1 total:3f6c...e1
20260216163954_baseline.sql 9a1b...04
20260217163957.sql 77d0...ac
```

* Line 1: `openmig-sum 1 total:<hex>`, where `<hex>` is the sha256 of all following lines, each
  including its trailing `\n`.
* Then one line per migration file, sorted by file name: `<file name> <sha256 hex of the file>`.
* Any added, removed or edited migration changes line 1, so two branches that both add a migration
  conflict in git and the second one must regenerate.
* `check` and `deploy` refuse a directory that does not match its sum file. `rehash` rewrites it.
  An empty directory needs no sum file.

## History table

Each database records applied migrations in a history table (default `openmig.history`, setting
`history_table`). The `openmig` schema/database is never inspected or diffed.

### PostgreSQL

```sql
CREATE SCHEMA IF NOT EXISTS "openmig";
CREATE TABLE IF NOT EXISTS "openmig"."history" (
  "version"             text PRIMARY KEY,
  "name"                text NOT NULL DEFAULT '',
  "checksum"            text NOT NULL,
  "kind"                text NOT NULL,          -- migration | baseline
  "transactional"       boolean NOT NULL,
  "statements_total"    integer NOT NULL,
  "statements_applied"  integer NOT NULL,
  "statement_checksums" text[] NOT NULL DEFAULT '{}',
  "status"              text NOT NULL,          -- applied | failed | running
  "error"               text,
  "error_statement"     integer,
  "started_at"          timestamptz NOT NULL,
  "finished_at"         timestamptz,
  "applied_by"          text NOT NULL
);
COMMENT ON TABLE "openmig"."history" IS 'openmig-history v1';
```

* Transactional files: the history row is written inside the file's transaction, so a file is
  either fully applied and recorded or not at all.
* Lock: a session-level advisory lock `pg_advisory_lock(hashtext('openmig:' || current_database()))`
  held by the deploying session; waiting polls `pg_try_advisory_lock` until `lock_wait` expires.
  Connection poolers in transaction mode (PgBouncer) break session locks; deploy over a direct
  connection.

### ClickHouse

```sql
CREATE DATABASE IF NOT EXISTS `openmig` [ON CLUSTER c];
CREATE TABLE IF NOT EXISTS `openmig`.`history` [ON CLUSTER c] (
  `version` String, `name` String, `checksum` String, `kind` LowCardinality(String),
  `transactional` Bool, `statements_total` UInt32, `statements_applied` UInt32,
  `statement_checksums` Array(String), `status` LowCardinality(String),
  `error` Nullable(String), `error_statement` Nullable(UInt32),
  `started_at` DateTime64(3, 'UTC'), `finished_at` Nullable(DateTime64(3, 'UTC')),
  `applied_by` String, `updated_at` DateTime64(6, 'UTC')
) ENGINE = [Replicated]ReplacingMergeTree(`updated_at`) ORDER BY `version`
COMMENT 'openmig-history v1';
```

* The engine is replicated when `cluster` is set. Reads use `FINAL`; on a replicated table they add
  `select_sequential_consistency = 1` and writes use an insert quorum of a majority of the table's
  replicas with `insert_quorum_parallel = 0` (an explicit number: `insert_quorum = 'auto'` never
  confirms on a single replica). Each write inserts a new version of the row with a strictly
  increasing `updated_at`; history repair deletes a row with a lightweight `DELETE`.
* Lock: when Keeper is available, a table `<history database>.lock` with
  `ENGINE = KeeperMap('/openmig/<history database>/lock')` (`key`, `holder`, `acquired_at`); the
  lock is taken by inserting a row with `keeper_map_strict_mode = 1` (fails if present) and
  released by deleting it. The row names the holder (`openmig/<version> host=... pid=...
  user=...`) and when it was taken; a wait that times out reports both, with the `DELETE` that
  removes a lock left behind by a killed deploy. Without Keeper a warning is printed and only one
  deploy may run at a time.

## Deploy algorithm

1. Connect (retrying until `--wait-for-db` expires), take the lock, ensure the history table, load
   history and the directory, verify `openmig.sum`.
2. History empty:
   * with `--baseline V`: record every file up to `V` as `baseline` (not executed). `V` must name a
     file. The option is honoured while the history holds only baseline rows (an interrupted
     baseline completes on rerun) and ignored once a migration has been applied;
   * otherwise, if the database has objects in scope and `--allow-dirty` is not given: refuse.
3. Refuse if an applied or baselined file's checksum changed or the file is missing, if a
   `failed`/`running` file with applied statements is missing (one with none applied may be deleted),
   or if a pending file is older than the newest applied one (unless `allow_out_of_order`). Every
   pending file is split before anything runs, so syntax errors stop the deploy early.
4. For each pending file (up to `--to`, which must name a file):
   * **Transactional:** `BEGIN; SET LOCAL lock_timeout = '<lock_timeout>'; <statements>;
     <history row: applied>; COMMIT`. On a lock timeout: roll back, wait with exponential backoff,
     retry (up to `retries`). On any other error: roll back, record a `failed` row (outside the
     transaction) and stop. The wait before retry `n` (from 0) is `1s * 2^n`, at most 30s.
   * **Non-transactional:** record a `running` row; after each statement update
     `statements_applied` and `statement_checksums`; on success mark `applied`; on error mark
     `failed` with `error_statement` and stop. A rerun resumes after the applied prefix if the
     checksums of those statements are unchanged, and refuses otherwise (also when the file lost
     its `no-transaction` directive).
   * A `statement-timeout` directive applies in both modes; the lock timeout only to transactional
     files.
   * `RESET ALL` between files.
   * `--dry-run` takes no lock and writes nothing: it reads history (if the table exists), runs the
     same checks and reports what would run.
5. Release the lock; print a summary (JSON with `--log-format json`).
