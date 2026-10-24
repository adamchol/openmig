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
`Recreate`. `plan` turns changes into ordered statements:

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

* Tables are never rebuilt. Columns: `ADD/DROP COLUMN`, `ALTER COLUMN TYPE ... USING`,
  `SET/DROP NOT NULL`, `SET/DROP DEFAULT`, identity changes, `SET EXPRESSION` (17+).
* Functions: `CREATE OR REPLACE` when input argument types, return type and argument names are
  unchanged and no defaults are removed (keeps grants, owner, comment); otherwise drop + create,
  rebuild dependants, re-grant.
* Views: `CREATE OR REPLACE VIEW` when the old columns remain a prefix with the same names and types;
  otherwise drop + create with dependants.
* Policies: `ALTER POLICY` unless `for` or `as` changes (then drop + create). Triggers:
  `CREATE OR REPLACE TRIGGER` (14+).
* Indexes, checks, foreign keys, unique constraints: drop + create on change.
* Enums: `ADD VALUE [BEFORE|AFTER]`. Removing or reordering values is unsupported.
* Unsupported (error with a hint to write the migration by hand with `openmig new`): changing a
  generated column's expression before 17, partitioning changes, removing enum values.
* Owners are not managed; owner differences are ignored.

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
