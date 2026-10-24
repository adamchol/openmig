# Architecture

## Crates

| Crate | Responsibility | Depends on |
|---|---|---|
| `openmig` | lib + bin. Flows (`sync`, `generate`, `check`, `deploy`, `status`, `diff`, `inspect`, `new`, `rehash`), the `Ui` trait, plan rendering, watch loop, clap CLI, logging, exit codes. | everything |
| `openmig-config` | `openmig.toml`, lazy `${ENV}` expansion, discovery. Engine settings stay raw TOML. | – |
| `openmig-hcl` | Parse (hcl-edit), evaluate (variables, locals, functions, `for_each`, references), `Document`, `BlockDecoder`, diagnostics (miette), HCL writer. | – |
| `openmig-scratch` | Start/stop throwaway PostgreSQL clusters and ClickHouse servers. | – |
| `openmig-core` | `ObjectRef`, `Plan`/`Statement`, `DepGraph`, `ExcludeSet`, renames, `HistoryRow`, `Error`, SQL lexer/splitter, the `Engine` and `Session` traits. | hcl, scratch |
| `openmig-migrate` | Migration directory, `openmig.sum`, directives, the generic deploy executor, `replay`, `status`. | core |
| `openmig-postgres` | PostgreSQL engine: `model`, `inspect`, `decode`, `encode`, `diff`, `plan`, `sql`, `acl`, `data`, `history`. | core, hcl, scratch |
| `openmig-clickhouse` | ClickHouse engine over HTTP; same modules. | core, hcl, scratch |
| `openmig-testkit` | Shared scratch servers for tests and the golden-case runner. | core, hcl, scratch |

The flows are generic over `E: Engine` (`openmig-core/src/engine.rs`). The CLI matches on the
configured engine and calls the generic flow with `PostgresEngine` or `ClickHouseEngine`.

## Normalization through a scratch database

HCL is written in "natural" form; databases store definitions in canonical form (expressions are
re-printed, view queries rewritten, defaults cast, ClickHouse engines expanded). Comparing HCL with an
inspected database directly would report spurious differences, so the desired state is always
normalized by the database itself:

1. Decode HCL into the engine model `D0`.
2. Plan `empty -> D0` with `PlanMode::Scratch` and apply it to a scratch database:
   * PostgreSQL: one transaction, `SET check_function_bodies = off`, a savepoint around each
     statement. A statement failing with an "undefined object" error is rolled back to its savepoint
     and moved to the end of the queue; the loop ends when a full pass makes no progress. Remaining
     errors are reported at the HCL location of the statement's `origin`.
   * ClickHouse: statement by statement, with the same retry queue.
3. Insert `data` rows.
4. Inspect the scratch database into the normalized desired model `D`.

ClickHouse keeps parentheses written around a whole expression (`DEFAULT (now())`,
`ORDER BY ((a), (b))`), so inspection strips redundant outer parentheses from key elements,
partition and sample expressions, TTLs, column defaults, index and constraint expressions, row
policy filters and function bodies. View and materialized view queries are compared as the
server prints them; it inlines SQL functions into them and into column expressions.

Sessions use fixed settings so that both sides print identically. PostgreSQL inspection sessions:
`search_path = ''` (every name fully qualified), `TimeZone = 'UTC'`, `DateStyle = 'ISO'`,
`IntervalStyle = 'postgres'`, `extra_float_digits = 3`. Apply sessions (scratch apply and deploy) use
the configured `search_path`.

## Flows

* **sync**: inspect target -> `C`; normalize schema -> `D`; resolve renames; `diff(C, D)`; plan
  (`PlanMode::Sync`); confirm destructive statements; execute (one transaction unless a statement
  forbids it); inspect target again and diff with `D` — must be empty, otherwise report a
  non-convergent change (a bug).
* **generate**: replay the migration directory into scratch -> `C`; reset scratch; normalize schema
  -> `D`; renames from hints only (plus interactive prompts on a TTY); `diff(C, D)`; plan
  (`PlanMode::Generate`); write the file and update the sum; reset scratch, replay everything
  including the new file, inspect, diff with `D` — must be empty.
* **check**: validate names, directives and the sum; replay into scratch -> `C`; normalize -> `D`;
  `diff(C, D)` must be empty, else print the missing plan and exit 4.
* **deploy**: see `formats.md`.
* **status --drift**: replay into scratch -> `M`; inspect target -> `C`; `diff(M, C)` must be empty.

## Diff and plan

`diff` compares two models object kind by object kind, keyed by identity, in deterministic
(`BTreeMap`) order, and produces engine `Change`s: `Create`, `Drop`, `Alter { ops }`, `Rename`,
`Recreate` (PostgreSQL adds `Data` for rows). Changes name objects by key; the planner looks
their definitions up in the two models. `Create` and `Drop` cover an object with everything
nested in it. `plan` turns changes into ordered statements:

1. **Expand blocked changes.** Some changes are blocked by dependants that did not change
   themselves (a column type change used by a view or policy; a function that must be dropped and
   recreated because its signature changed and is used by a default, check or view). Using the
   *current* dependency graph (PostgreSQL `pg_depend`, ClickHouse `system.tables` dependencies) the
   planner drops those dependants before the change and recreates them after.
2. **Order.** Each step provides and requires object keys. A `DepGraph` sorts steps with Kahn's
   algorithm and a priority of (phase, kind rank, key):
   * creation rank: roles, extensions, schemas, types (enum, composite, domain), sequences,
     functions needed by tables, tables (without cyclic foreign keys), indexes, constraints and
     foreign keys, views (topologically), materialized views, remaining functions and procedures,
     triggers, event triggers, RLS and policies, grants, comments, data;
   * drops run after creations unless a dependency forces them earlier; dropping X happens before
     dropping Y when X depends on Y; recreating a name happens after dropping it;
   * foreign keys that form a cycle are added with `ALTER TABLE ... ADD CONSTRAINT` after all tables;
   * dependencies guessed from SQL text (identifiers in function bodies and view queries) are soft
     edges, dropped if they would create a cycle.
3. **Transaction mode.** A statement that cannot run in a transaction (`CONCURRENTLY`, using an enum
   value added in the same plan) makes the whole migration `no-transaction`.

### PostgreSQL planning rules

* Tables are never rebuilt. Columns: `ADD/DROP COLUMN`, `ALTER COLUMN TYPE ... USING col::type`
  (with `COLLATE` for collation changes; an existing default is dropped first and set again),
  `SET/DROP NOT NULL`, `SET/DROP DEFAULT`, identity changes (`ADD GENERATED ... AS IDENTITY`,
  `SET GENERATED`, `SET <sequence option>`, `DROP IDENTITY`), `SET EXPRESSION` (17+, virtual
  columns 18+), `DROP EXPRESSION` (stored generated column to plain column), `SET STORAGE`,
  `SET COMPRESSION`. Column order differences are ignored. Columns of partitions, and columns an
  `INHERITS` child gets from its parents, are managed through the parent.
* `CREATE TABLE` carries the columns, primary key, unique constraints (except one on the same
  columns as the primary key or an earlier unique constraint, which PostgreSQL would silently
  drop), valid checks, exclusion constraints and foreign keys to tables that exist or are created
  earlier outside a foreign-key cycle. Everything else (cyclic foreign keys, `NOT VALID`
  constraints, indexes, policies, triggers, RLS, replica identity, comments, grants) follows as
  separate statements. Partitions are created `PARTITION OF` their parent; a changed bound or
  parent is `DETACH PARTITION` + `ATTACH PARTITION`; `INHERITS` parents are added and removed with
  `INHERIT` / `NO INHERIT`.
* Functions and procedures: `CREATE OR REPLACE` when the arguments (modes, types and names, hence
  also `OUT` parameters), the result and the kind (`WINDOW`) are unchanged and no argument default
  is removed (keeps grants, owner, comment); `ALTER FUNCTION` when only attributes (volatility,
  strictness, security, leakproof, parallel, cost, rows, `SET`) change; otherwise drop + create,
  rebuild dependants, re-grant.
* Views: `CREATE OR REPLACE VIEW` when the old columns remain a prefix with the same names and types
  (options are replaced with it); `ALTER VIEW ... SET/RESET (...)` when only options change;
  otherwise drop + create with dependants. Materialized views: drop + create (with their indexes)
  when the query changes, `ALTER MATERIALIZED VIEW ... SET/RESET` for storage parameters,
  `REFRESH MATERIALIZED VIEW [WITH NO DATA]` for the populated state.
* Policies: `ALTER POLICY` for roles, `USING` and `WITH CHECK`, unless `for` or `as` changes or a
  `USING` / `WITH CHECK` clause is removed (then drop + create). Triggers:
  `CREATE OR REPLACE TRIGGER` (14+), except constraint triggers (drop + create); the firing mode
  (`ENABLE`, `DISABLE`, `ENABLE REPLICA`, `ENABLE ALWAYS`) is set again after a replace.
* Indexes, checks, foreign keys, unique, primary key and exclusion constraints: drop + create on
  change; `VALIDATE CONSTRAINT` when a `NOT VALID` constraint becomes valid. Domain checks
  likewise with `ALTER DOMAIN`.
* With `concurrent_indexes`, index creation and removal on tables and materialized views that
  exist before and after the plan (and are not partitioned) use `CONCURRENTLY`; in `generate`
  each creation is preceded by `DROP INDEX CONCURRENTLY IF EXISTS` so that a failed file can be
  retried. Never in scratch builds.
* Sequences: `ALTER SEQUENCE` with every option (so that a type change does not move the bounds);
  `OWNED BY` as a separate statement after the owning column exists. Sequences owned by a dropped
  table or column are dropped by PostgreSQL with it.
* Enums: `ADD VALUE [BEFORE|AFTER]`, in declared order. Removing or reordering values recreates
  the type (with its dependants) when no table column uses it; otherwise unsupported.
* Composite types: `ADD/DROP ATTRIBUTE` and `ALTER ATTRIBUTE ... TYPE` in place when the resulting
  attribute order is the desired one; reordering recreates the type when no table column uses it.
  Retyping an attribute while a table column uses the type is unsupported. Domains: default,
  `NOT NULL` and checks in place; a changed base type or collation recreates the domain when no
  table column uses it, otherwise unsupported.
* Roles (only with `manage_roles`, and always in scratch builds): `CREATE ROLE`, `ALTER ROLE` for
  attributes, memberships with `GRANT role TO member WITH ADMIN|INHERIT|SET TRUE` (16+; only
  `ADMIN OPTION` before) and `REVOKE ... OPTION FOR`. An expiry cannot be removed, so `VALID UNTIL
  'infinity'` and no expiry compare equal.
* Privileges: only managed grantees are compared (`hcl.md`). A created (or recreated) object starts
  from PostgreSQL's default ACL for its kind. `MAINTAIN` is ignored before PostgreSQL 17.
* Extensions: `CREATE EXTENSION IF NOT EXISTS ... WITH SCHEMA`; `VERSION` only in scratch builds
  and only when declared; versions are never compared; `ALTER EXTENSION ... SET SCHEMA`. The
  comment `CREATE EXTENSION` sets is only changed when the desired side has one.
* Data: rows are compared by the desired key on the declared columns, as PostgreSQL text. Values
  are written as string literals that PostgreSQL casts; identity `ALWAYS` columns are inserted with
  `OVERRIDING SYSTEM VALUE`. Rows run after every other change of their table; referenced tables
  get their inserts and updates before, and their deletes after, the referencing tables.
* Unsupported (error with a hint to write the migration by hand with `openmig new`, or to allow
  recreating with `sync --recreate`): changing a generated column's expression before 17, a plain
  column becoming generated, an identity column becoming generated, partitioning changes,
  removing or reordering enum values and the other type changes above while table columns use the
  type. With `allow_recreate` the column or table is dropped and created again (destructive).
* Owners are not managed; owner differences are ignored.

#### Steps and ordering

Each change becomes steps: statements plus the objects the step *creates* (exist with their
desired definition afterwards), *removes* (dropped, or changed in a way dependants block),
*releases* (whose current dependencies it gives up: drops and redefinitions) and *requires*.
Requirements come from the model's structure (schemas, relations, column types, foreign keys,
trigger and event trigger functions, policy roles, grantees, sequence owners, partition parents)
and from the desired model's `dependencies`. Edges:

* a step creating X runs before the steps requiring X;
* a step releasing X runs before the steps removing what X depends on (current dependencies);
* a step removing X runs before a step creating an object that takes the same name (relations,
  indexes and composite types share one namespace per schema, types another, constraints are
  unique per table);
* renames run before every other step on the renamed object; rows run after every other step on
  their table; revokes run before the grantee role is dropped;
* soft edges from identifiers in SQL text (function bodies always; defaults, checks, index
  expressions, policies, trigger conditions and view queries in scratch builds, whose decoded
  model has no dependencies) are dropped when they would form a cycle.

Blocked dependants: when an object is dropped or recreated, every current dependant that the plan
does not drop or recreate itself is dropped before and created after it (from the desired model,
with its comments, grants and nested triggers or indexes). A dependant that is redefined in place
(`CREATE OR REPLACE`, `ALTER POLICY`) does not need this when the object is dropped for good.
For a column type or expression change, PostgreSQL rebuilds indexes and constraints itself, so
only views, materialized views, policies, triggers, generated columns and functions with
SQL-standard bodies are rebuilt. Column and domain defaults using a recreated function are
dropped and set again; generated columns are dropped and added again (their values are
recomputed). A table column whose type would have to be dropped is unsupported.

Plans that create SQL-language functions with string bodies start with
`SET check_function_bodies = off`, in every mode: such bodies may reference objects created later
in the plan, and their references are not recorded in `pg_depend`.

Scratch builds plan no drops: the pristine database only has the `public` schema, which the
decoded model always declares. Roles named by the decoded model but not declared
(`external_roles`) are created `NOLOGIN` when missing.

### ClickHouse planning rules

* `ALTER TABLE`: `ADD/DROP/MODIFY/RENAME COLUMN`, codec, default kind/expression, column and table
  `TTL`, `MODIFY/RESET SETTING`, `ADD/DROP INDEX`, `MODIFY COMMENT`.
* Unsupported: changing the engine, `ORDER BY` (except appending newly added columns), `PRIMARY KEY`,
  `PARTITION BY`, or the type of a key column.
* Materialized views with `TO`: drop + create (no data loss).
* `DROP TABLE ... SYNC`. DDL runs with `alter_sync = 2`, `mutations_sync = 2`.
* Cluster rules: see `hcl.md`.

## Renames

* Hints: `renamed_from` on the desired object. Applied when the old object exists in the current
  model and is absent from the desired model, and the new object is absent from the current model.
  The current model is re-keyed before diffing, so a rename plus another change works.
* PostgreSQL: tables and columns. A table moved to another schema is `SET SCHEMA` (which also moves
  its indexes, constraints and owned sequences) followed by `RENAME TO`. Column hints may name the
  table by its old or new name. Re-keying rewrites what PostgreSQL rewrites itself: foreign keys,
  partition and inheritance parents, sequence owners, column lists of constraints and indexes,
  unqualified references to the renamed column in expressions of its table, and `nextval` defaults
  of moved sequences.
* Interactive (TTY only, `sync` and local `generate`): drop + create pairs of the same kind in the same
  container are offered as renames — columns with the same type and nullability, tables whose column
  sets overlap by at least 70%. Accepting prints the `renamed_from` line to add to the schema. Without
  a TTY nothing is guessed.

## Scratch servers

* PostgreSQL: one private cluster per command (`initdb` output cached per binary version and args),
  unix socket only, `fsync = off`. Reset: `DROP DATABASE ... WITH (FORCE)`, `CREATE DATABASE`, drop
  every role created since start. Roles never collide with anything because the cluster is private.
* ClickHouse: one server with embedded Keeper, a single-node cluster and macros. Reset: drop every
  non-system database (`SYNC`) and every role/user created since start.
* In `--watch` mode the scratch server lives for the whole session.

Details (`openmig-scratch`):

* Every server gets a directory `/tmp/openmig-pg-XXXXXX` or `/tmp/openmig-ch-XXXXXX` (short, because
  unix socket paths are limited to about 107 bytes) holding its data, socket, logs and a pid file
  naming the OpenMig process. Starting a server removes directories whose pid file names a dead
  process.
* Servers run in their own process group and are forked from one long-lived thread with
  `PR_SET_PDEATHSIG = SIGKILL` (set after the user switch, which would clear it), so they die with
  OpenMig even when it is killed. Stop: PostgreSQL `SIGQUIT` (immediate shutdown), ClickHouse
  `SIGTERM`; `SIGKILL` after a grace period or on drop.
* PostgreSQL: `initdb -U openmig --auth=trust -N --encoding=UTF8 --locale=C.UTF-8`; the pristine data
  directory is cached in `$XDG_CACHE_HOME/openmig/pg/<hash>/data` (hash of the binaries, version,
  arguments and locale/`TZ` environment), populated by an atomic rename and copied for later starts.
  `initdb` and `postgres` refuse to run as root, so as root they run as `run_as` (default `nobody`)
  and the socket is created with mode 0777. Ready when `postmaster.pid` reports `ready`. URL:
  `postgresql:///postgres?host=<dir>&port=5432&user=openmig`.
* ClickHouse: `clickhouse server` with a generated `config.xml` on kernel-picked loopback ports
  (retried on a port conflict): embedded Keeper with fast single-node election, `<zookeeper>`,
  macros `shard = 01`, `replica = r1`, `cluster = <cluster>`, a one-shard `remote_servers` cluster,
  `distributed_ddl`, `keeper_map_path_prefix`, user directories `users_xml`, `local_directory` (the
  default for `CREATE ROLE/USER`) and `replicated`, each configured storage policy mapped to the
  default disk, `enable_read_write_grants`, `UTC` time zone, small caches and thread pools. System
  log tables are simply not configured (ClickHouse only creates the configured ones). The `default`
  user has no password and every privilege including access management and named collections.
  Ready when `/ping` answers and `system.zookeeper` can be read. URL:
  `http://default@127.0.0.1:<http port>/`.

Tests lease servers from process-wide pools in `openmig-testkit` (`postgres()`, `clickhouse()`;
sizes `OPENMIG_TEST_PG_SERVERS` = 2 and `OPENMIG_TEST_CH_SERVERS` = 1, PostgreSQL major version
`OPENMIG_TEST_PG_VERSION`). A returned server is reset before its next lease.
