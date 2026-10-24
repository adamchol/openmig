# OpenMig schema language (HCL)

Schemas are written in [HCL](https://github.com/hashicorp/hcl/blob/main/hclsyntax/spec.md) native
syntax. This document is the reference for both engines. Engine-specific blocks are in
[PostgreSQL](#postgresql) and [ClickHouse](#clickhouse).

## Files

* Every `*.hcl` file under the paths in `schema = [...]` (directories are walked recursively, in
  sorted order) plus `--schema PATH` overlays is loaded into one namespace. File boundaries do not
  matter: any block can reference any other.
* Declaring the same object twice is an error that shows both locations. The exception is `data`
  blocks: several blocks for the same table merge their rows.

## Labels and names

* Objects that live in a schema (PostgreSQL) or database (ClickHouse) use a **qualified label**:
  `table "app.users"`. The label is split on the first dot. A label without a dot gets the default
  schema (`default_schema` setting, `public` for PostgreSQL) or database (`default` for ClickHouse).
* Cluster-wide objects and nested objects use a plain label: `role "web"`, `column "email"`.
* Labels are literal strings (no interpolation). `name = "..."` overrides the name used in the
  database, e.g. `database "dialo" { name = var.dialo_db }` or several overloads of one function.
  For a qualified block, `name` replaces only the object name; `schema = schema.x` (PostgreSQL) /
  `database = database.x` (ClickHouse) replaces the namespace. Use these for names containing dots.
  Nested blocks (`column`, `index`, constraints, ...) accept `name` too, which is how `for_each`
  instances get distinct names.

## References

A reference names a referenceable block: `<kind>.<label parts>[.<nested kind>.<label>]`.

| Reference | Meaning |
|---|---|
| `schema.app` | schema `app` |
| `table.app.users` | table `app.users` |
| `table.users` | table `users` in whichever schema, if exactly one exists (else an error listing candidates) |
| `table.app.users.column.id` | column `id` of `app.users` |
| `column.id` | inside a table: column `id` of the enclosing table |
| `index.users_email_idx` | inside a table: an index of the enclosing table |
| `function.app.current_user_id` | a function (by label) |
| `role.s3-uploader` | role `s3-uploader` (hyphens are allowed in identifiers) |
| `role["odd.name"]` | index syntax for labels that are not identifiers |
| `"${table.app.users.name}"` | `.name` of a reference is the object's database name, for interpolation |

Referencing a block that does not exist is an error with a did-you-mean suggestion. A block produced
by `for_each` cannot be referenced by label (its instances share the label).

Where a list of columns is expected, plain strings also work: `columns = ["tenant_id", "id"]`.

## Values

* **Literals**: strings, numbers, `true`/`false`, `null`, lists `[...]`, objects `{ k = v }`.
* **Bare identifiers** that are not variables or block kinds are kept as identifiers. They are used
  for keywords (`for = ROW`, `on_delete = CASCADE`, `privileges = [SELECT]`, `to = [PUBLIC]`) and
  types (`type = text`). Keywords are case-insensitive.
* **Calls of unknown functions** are kept as calls and rendered to SQL by the engine:
  `type = varchar(255)`, `type = LowCardinality(String)`, `codec = [Delta(8), ZSTD(1)]`,
  `engine = ReplicatedMergeTree("/clickhouse/tables/{uuid}/{shard}", "{replica}")`. String arguments
  become SQL string literals, identifier arguments stay identifiers, numbers stay numbers.
* **Strings for types** that HCL cannot express: `type = "text[]"`, `type = "DateTime64(9, 'UTC')"`,
  `type = "Enum8('a' = 1, 'b' = 2)"`.
* **`sql("...")`** marks raw SQL where a plain string would be a literal value: `default`, values in
  `data` rows. Attributes that are always SQL take plain strings (`as`, `body`, `using`, `check`,
  `where`, `expr`, `when`, `generated`, `partition_by`, `order_by` entries given as strings, ...).
* **Heredocs**: `<<-SQL ... SQL` strips the common leading indentation; `<<SQL` keeps it. As in
  HCL, the value ends with the newline before the closing marker; `chomp(<<-SQL ... SQL)` drops it.
* **Templates**: `"${var.x}"`; write `$${` for a literal `${`.
* Comments: `#`, `//` and `/* */`.

## Variables, locals, functions, loops

```hcl
variable "dialo_db" {
  type    = string        # string | number | bool | list(...) | map(...) | any
  default = "dialo"
  description = "Database name"
}

locals {
  app_roles = [role.web, role.worker]
}
```

* `var.<name>` / `local.<name>`. A variable without a default must be given a value.
* Values come from `--var name=value` (highest priority; converted to the declared type, so
  `--var replicated=false` is a bool), then `[databases.<db>.vars]` in `openmig.toml`, then
  `default`.
* Locals may reference variables, other locals and blocks; cycles are errors.
* Functions (pure, no environment access):
  `format`, `join`, `split`, `lower`, `upper`, `replace`, `trimspace`, `trimprefix`, `trimsuffix`,
  `chomp`, `concat`, `flatten`, `distinct`, `contains`, `keys`, `values`, `merge`, `coalesce`,
  `length`, `range`, `tostring`, `tonumber`, `tobool`, `try`, `jsonencode`, `jsondecode`, `file`,
  `sql`. `file("x.sql")` reads a file relative to the HCL file that calls it. Any other function
  name is kept as a call (see Values), so these names cannot be used for SQL types or engines.
* Objects keep the order in which they are written: `keys`, `values`, `for` expressions and
  `for_each` iterate in that order.
* Operators: arithmetic, comparison, `&&`, `||`, `!`, conditionals `c ? a : b`, `for`
  expressions `[for r in local.app_roles : r]`, splat is not supported.
* `for_each = <list or map>` on any block creates one instance per element. `each.value` is the
  element, `each.key` the map key or list index. There is no `count` and no `dynamic`. Nested
  blocks of an instance can use `each` too; a nested `for_each` shadows the outer one. Instances
  (and blocks nested in them) cannot be referenced, not even relatively (`column.id`): use plain
  names such as `columns = ["id"]` inside them.

## Common attributes

| Attribute | Applies to | Meaning |
|---|---|---|
| `comment` | most objects | Object comment (`COMMENT ON`, ClickHouse `COMMENT`). `""` is no comment. |
| `depends_on` | most objects | Extra ordering dependencies: list of references. Rarely needed; dependencies are normally discovered automatically. |
| `renamed_from` | tables, columns (P0); schemas, views, materialized views, sequences, enums, composites, domains, indexes (P1) | Previous name. When the old object exists and the new one does not, the plan renames instead of drop + create. Ignored once the old name is gone, so it can stay in the schema until convenient to remove. For schema objects it may be qualified (`"app.people"`) to also move schemas; unqualified, the old object is in the same schema. |

---

## PostgreSQL

Supported servers: PostgreSQL 14–18. Default schema: `public` (setting `default_schema`).

**Schemas.** The default schema and `public` are always part of the schema, and so is every
schema an object's name uses; a `schema` block is only needed for an empty schema or to set
attributes. `public` comes with what every new database gives it: the comment `standard public
schema` and `USAGE` for `PUBLIC` (none with `public_defaults = "revoke"`); `comment = ""` removes
the comment, a `revoke` block the privilege. On PostgreSQL 14 `public` also grants `CREATE` to
`PUBLIC`, which a schema without a matching `grant` revokes. System schemas (`pg_*`,
`information_schema`) cannot be declared.

**Types.** Builtin types are normalized to the names PostgreSQL prints: `int`, `int4` and
`integer` are `integer`; `int8`/`bigint`, `int2`/`smallint`, `float8`/`float`/`double_precision`,
`float4`/`real`, `bool`/`boolean`, `decimal`/`numeric` (`numeric(10)` = `numeric(10,0)`),
`varchar`/`character_varying`, `char`/`character` (`char` = `character(1)`), `timestamp`,
`timestamptz`, `time`, `timetz` (with an optional precision: `timestamptz(3)`), `varbit`, `bit`
(`bit` = `bit(1)`), `float(p)`. Other identifiers and calls are kept (lower-cased), strings
verbatim. A reference to an `enum`, `composite`, `domain`, `table` or `view` becomes the
qualified name, quoted where PostgreSQL would quote it (`app."user"`). Function argument and
result types drop modifiers, as PostgreSQL does (`varchar(10)` is `character varying`).

**Literals.** `default` values: a string becomes a literal cast to the column type without
modifiers, as PostgreSQL prints it (`"active"` on an `enum.app.status` column is
`'active'::app.status`, `"x"` on `varchar(10)` is `'x'::character varying`); numbers and bools are
written as is; `sql("...")` is raw SQL.

**Collations** (`collate`): a name, quoted as an identifier where needed (`"C"` is `COLLATE "C"`),
or raw SQL with `sql("...")` for a qualified collation (`sql("public.my_coll")`).

### `extension "name"`

| Attribute | Type | Notes |
|---|---|---|
| `schema` | schema ref or string | Schema to install into (`WITH SCHEMA`). |
| `version` | string | Compared only when declared. |
| `comment` | string | |

### `schema "name"`

| Attribute | Type | Notes |
|---|---|---|
| `name` | string | Database name if different from the label. |
| `comment` | string | |

### `table "schema.name"`

| Attribute / block | Type | Notes |
|---|---|---|
| `column "name" {}` | block, repeated | Column order is the declaration order. Order differences in an existing table are ignored. |
| `primary_key {}` | block | Optional label = constraint name (default `<table>_pkey`). |
| `foreign_key "name" {}` | block | |
| `unique "name" {}` | block | Unique *constraint* (see `index` for unique indexes). |
| `check "name" {}` | block | |
| `index "name" {}` | block | Label optional (PostgreSQL names unlabelled indexes). |
| `policy "name" {}` | block | Row-level security policy on this table. |
| `trigger "name" {}` | block | Trigger on this table. |
| `exclude "name" {}` | block | P1, exclusion constraint. |
| `rls` | bool or `{ enabled = bool, force = bool }` | `true` = ENABLE ROW LEVEL SECURITY; `force` = FORCE. In the object form `enabled` defaults to true. |
| `replica_identity` | `DEFAULT`, `FULL`, `NOTHING`, `index.<name>` or an index name string | A string names an index, e.g. the primary key's `"users_pkey"`. |
| `unlogged` | bool | P1 |
| `storage_params` | object | P1, `WITH (fillfactor = 70)` |
| `partition_by` | string | P1, `PARTITION BY` clause: `"RANGE (created_at)"`. |
| `partition_of`, `partition_bound` | table ref, string | P1, a partition: `partition_of = table.app.events`, `partition_bound = "FOR VALUES FROM ('2026-01-01') TO ('2027-01-01')"` or `"DEFAULT"`. Columns are declared as in the parent. |
| `inherits` | list of table refs | P1, `INHERITS (...)`. Inherited columns are declared too. |
| `comment`, `renamed_from`, `depends_on` | | |

#### `column "name"`

| Attribute | Type | Notes |
|---|---|---|
| `type` | type | Required. Identifier (`text`, `bigint`, `timestamptz`, `double_precision` = `double precision`, `character_varying` = `character varying`), call (`varchar(255)`, `numeric(10, 2)`, `timestamp(3)`), string (`"text[]"`, `"public.citext"`), or a reference to `enum.*`, `domain.*`, `composite.*`, `table.*` (row type). See **Types** above. |
| `null` | bool | Default `true` (nullable), as in SQL. Primary-key, identity and serial columns are NOT NULL automatically (`null = true` is an error for identity and serial columns). |
| `default` | literal or `sql()` | `default = 0`, `default = "active"` (string literal), `default = sql("now()")`. At most one of `default`, `identity` and `generated`. |
| `identity` | `BY_DEFAULT`, `ALWAYS`, or `{ generated = ALWAYS, start = 1, increment = 1, min = 1, max = 100, cache = 1, cycle = false }` | Identity column. In the object form `generated` defaults to `BY_DEFAULT`; omitted parameters take PostgreSQL's defaults for the column type (descending sequences start at their maximum, -1). |
| `generated` | string | `GENERATED ALWAYS AS (expr) STORED`. P1: `{ expr = "...", virtual = true }` on PostgreSQL 18. |
| `collate` | string | `"C"`, `"en_US"`; see **Collations**. |
| `storage` | `PLAIN`, `EXTERNAL`, `EXTENDED`, `MAIN` | P1, `SET STORAGE` when not the type's default. |
| `compression` | identifier or string | P1, `pglz` or `lz4`. |
| `comment`, `renamed_from` | | |

`serial`, `bigserial` and `smallserial` (and `serial4`, `serial8`, `serial2`) are accepted as types:
they become an integer column with an owned sequence `<table>_<column>_seq` (named, shortened and
de-duplicated like PostgreSQL does) and a `nextval('<schema>.<sequence>'::regclass)` default. The
sequence has no block and cannot be referenced; to grant privileges on it, declare the `sequence`
with `owned_by`, give the column its integer type and write the `nextval` default.

#### `primary_key`

| Attribute | Type | Notes |
|---|---|---|
| `columns` | list of column refs/strings | Required. |
| `include` | list | P1 |
| `deferrable`, `initially_deferred` | bool | |
| `with` | object | P1, storage parameters of the index. |
| `comment` | string | |

The default name is `<table>_pkey`, shortened to 63 bytes like PostgreSQL does.

#### `foreign_key "name"`

| Attribute | Type | Notes |
|---|---|---|
| `columns` | list of column refs/strings | Required. |
| `references` | list of column refs | Required, e.g. `[table.app.users.column.id]`. All in one table. |
| `on_delete`, `on_update` | `NO_ACTION` (default), `RESTRICT`, `CASCADE`, `SET_NULL`, `SET_DEFAULT` | |
| `deferrable` | bool | |
| `initially_deferred` | bool | |
| `match` | `SIMPLE` (default), `FULL`, `PARTIAL` | P1 |
| `not_valid` | bool | P1, added `NOT VALID` and not validated. |
| `not_enforced` | bool | P1, `NOT ENFORCED` (PostgreSQL 18). |
| `comment` | string | |

The referenced table must be declared (references are resolved like any other reference).

#### `unique "name"`

`columns` (required), `nulls_distinct` (bool, default true; PostgreSQL 15+), `include` (P1),
`deferrable`, `initially_deferred`, `with` (P1, storage parameters), `comment`.

#### `check "name"`

`expr` (required, SQL), `no_inherit` (bool), `not_valid` (bool, P1), `not_enforced` (bool, P1,
PostgreSQL 18), `comment`.

#### `exclude "name"` (P1)

`using` (access method, default `btree`), `key { ... }` blocks as for indexes plus `operator`
(required, e.g. `"="`, `"&&"`), `where`, `include`, `deferrable`, `initially_deferred`, `with`,
`comment`.

#### `index "name"`

| Attribute / block | Type | Notes |
|---|---|---|
| `columns` | list of column refs/strings | Simple key columns. Either `columns` or `key` blocks. |
| `key {}` | block, repeated | One key part: `column = column.x` or `expr = "lower(email)"`, plus `desc = bool`, `nulls = FIRST\|LAST`, `opclass = jsonb_path_ops` (identifier or string), `collate = "C"`. |
| `unique` | bool | |
| `using` | `btree` (default), `hash`, `gin`, `gist`, `brin`, `spgist`, or any string | |
| `where` | string | Partial index predicate. |
| `include` | list | Covering columns. |
| `nulls_distinct` | bool | PostgreSQL 15+. |
| `with` | object | P1, storage parameters. |
| `comment`, `renamed_from` | string | |

An unlabelled index gets the name PostgreSQL would give it: `<table>_<key>[_<key>...]_idx`, where a
column key is the column name and an expression key is the function name of a call
(`lower(email)` gives `lower`), the column of a column or cast (`email::text` gives `email`), and
`expr` otherwise; names are shortened to 63 bytes and made unique with a number
(`users_email_idx1`) among the relations of the schema. Label indexes on more complex expressions.

With `concurrent_indexes = true` (setting), indexes added to or dropped from existing tables are
built with `CONCURRENTLY` and the migration becomes `no-transaction`.

#### `policy "name"` (nested in a table, or top-level with `on`)

Top-level `policy` and `trigger` blocks are labelled by name, and labels are unique per block kind:
when two top-level triggers (or policies) on different relations share a name, give them distinct
labels and set `name`.

| Attribute | Type | Notes |
|---|---|---|
| `on` | table ref | Only for top-level policies. |
| `for` | `ALL` (default), `SELECT`, `INSERT`, `UPDATE`, `DELETE` | |
| `to` | list of role refs, role names or `PUBLIC`, `CURRENT_USER`, `SESSION_USER`, `CURRENT_ROLE` | Default `[PUBLIC]`. Bare identifiers other than these keywords are errors: write `role.web` or `"web"`. |
| `as` | `PERMISSIVE` (default), `RESTRICTIVE` | |
| `using` | string | |
| `check` | string | `WITH CHECK`. |
| `comment` | string | |

#### `trigger "name"` (nested in a table, or top-level with `on`)

| Attribute | Type | Notes |
|---|---|---|
| `on` | table or view ref | Only for top-level triggers. |
| `before` / `after` / `instead_of` | list of `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE` | Exactly one of the three. |
| `update_of` | list of column refs/strings | `UPDATE OF ...`; implies `UPDATE`. |
| `for` | `ROW` or `STATEMENT` | Required. |
| `when` | string | `WHEN (...)`. |
| `execute` | function ref or `"schema.name"` | Required. A string names a function outside the schema (`"pg_catalog.tsvector_update_trigger"`). |
| `args` | list of strings | Trigger function arguments. |
| `constraint` | bool | P1, `CREATE CONSTRAINT TRIGGER`; then also `deferrable`, `initially_deferred` and `from` (table ref). |
| `referencing` | `{ old_table = "...", new_table = "..." }` | P1, transition tables. |
| `enabled` | bool, `ORIGIN`, `REPLICA`, `ALWAYS` or `DISABLED` | P1, default `true` (`ORIGIN`); `false` = `DISABLED`. |
| `comment` | string | |

### `view "schema.name"`

| Attribute | Type | Notes |
|---|---|---|
| `as` | string | Required. The query. Columns are derived from it. |
| `security_invoker` | bool | PostgreSQL 15+. |
| `security_barrier` | bool | |
| `check_option` | `LOCAL`, `CASCADED` | |
| `column "name" { comment = "..." }` | block | Only for column comments (column privileges are granted with `grant { columns = [...] }`). |
| `comment`, `depends_on`, `renamed_from` | | |

Leading and trailing whitespace and a trailing `;` of `as` are dropped. Triggers on views are
top-level `trigger` blocks with `on = view.<...>`.

`materialized_view "schema.name"` (P1): `as`, `with_data` (bool, default true), `storage_params`,
nested `index` (with column names as strings) and `column` (comments), `comment`.

### `function "schema.name"`

| Attribute / block | Type | Notes |
|---|---|---|
| `arg "name" {}` | block, repeated | `type` (required), `mode = IN\|OUT\|INOUT\|VARIADIC`, `default` (literal or `sql()`). The label may be omitted for unnamed arguments. |
| `returns` | type or reference | e.g. `bigint`, `"setof app.users"` is written as `returns = table.app.users` + `setof = true`; `trigger`, `event_trigger`, `void`. |
| `setof` | bool | `RETURNS SETOF`. |
| `returns_table { column "x" { type = ... } }` | block | `RETURNS TABLE (...)`. |
| `language` | `sql`, `plpgsql`, or any identifier | Required. |
| `body` | string | Required. Written with a dollar quote that does not occur in the body. A `sql` body starting with `RETURN` or `BEGIN ATOMIC` is a SQL-standard body, parsed when the function is created. |
| `volatility` | `VOLATILE` (default), `STABLE`, `IMMUTABLE` | |
| `strict` | bool | `RETURNS NULL ON NULL INPUT`. |
| `security` | `INVOKER` (default), `DEFINER` | |
| `leakproof` | bool | |
| `parallel` | `UNSAFE` (default), `RESTRICTED`, `SAFE` | |
| `cost`, `rows` | number | Defaults: cost 100 (1 for `c` and `internal`), rows 1000 for set-returning functions, else 0. |
| `window` | bool | P1, `WINDOW`. |
| `set` | object | `SET name = value` per entry, e.g. `{ search_path = "pg_catalog, public" }`. Values are the text PostgreSQL stores. |
| `name`, `comment`, `depends_on` | | |

Without `returns` and `returns_table`, the result is the type of a single `OUT`/`INOUT` argument
or `record` for several, as in PostgreSQL. Identity of a function is `(schema, name, input
argument types)`; two blocks with the same identity are an error. Overloads use distinct labels and
the same `name`.

### `procedure "schema.name"`

`arg`, `language`, `body`, `security`, `set`, `name`, `comment`, `depends_on` (as for functions).

### `event_trigger "name"`

`on` (`ddl_command_start`, `ddl_command_end`, `sql_drop`, `table_rewrite`, `login`), `tags` (list of
strings, e.g. `["CREATE TABLE"]`), `execute` (function ref), `enabled` (bool or `ORIGIN`,
`REPLICA`, `ALWAYS`, `DISABLED`; default true), `comment`.

### `enum "schema.name"`

`values` (list of strings, required), `comment`. New values are added with `ALTER TYPE ... ADD VALUE`
in the declared position. Removing or reordering values is an unsupported change.

### `composite "schema.name"`

`field "name" { type = ..., collate = "..." }` (repeated), `comment`.

### `domain "schema.name"`

`type` (required), `null` (bool, default true), `default`, `collate`, `check "name" { expr = "...",
not_valid = bool, comment = "..." }` (repeated), `comment`.

### `sequence "schema.name"`

`type` (`smallint`, `integer`, `bigint`; default `bigint`), `start`, `increment`, `min`, `max`,
`cache`, `cycle`, `owned_by` (column ref), `comment`. Omitted parameters take PostgreSQL's defaults
for the type and direction. Sequences backing identity and serial columns are part of the column and
are not declared. Current values are never compared.

### `role "name"`

| Attribute | Type | Notes |
|---|---|---|
| `login` | bool | Default false. A role with `login = true` is a user. |
| `superuser`, `createdb`, `createrole`, `inherit` (default true), `replication`, `bypassrls` | bool | |
| `connection_limit` | number | Default -1. |
| `valid_until` | string | Timestamp, e.g. `"2027-01-01T00:00:00Z"`, or `"infinity"`. |
| `member_of` | list of role refs or names, or `{ role = ..., admin = bool, inherit = bool, set = bool }` | Role memberships. Defaults: `admin = false`, `set = true`, `inherit` = the member's own `inherit` (as `GRANT` does on PostgreSQL 16+). |
| `comment` | string | |

Passwords are never part of the schema. Roles are cluster-wide: OpenMig manages them only with
`manage_roles = true`; otherwise role blocks only declare roles that grants and policies may name,
and they are created in scratch databases only.

### `grant` / `revoke`

```hcl
grant {
  privileges = [SELECT, INSERT, UPDATE]      # or [ALL]
  on         = [table.app.users, table.app.orgs]
  columns    = ["email"]                     # optional: column privileges
  to         = [role.web, PUBLIC]
  with_grant_option = false
}

revoke {
  privileges = [EXECUTE]
  on         = [function.app.internal_helper]
  from       = [PUBLIC]
}
```

`on` accepts references to schemas, tables, views, materialized views, sequences, functions,
procedures, domains, enums and composites (types). Privileges: `SELECT`, `INSERT`, `UPDATE`, `DELETE`,
`TRUNCATE`, `REFERENCES`, `TRIGGER`, `MAINTAIN` (17+), `USAGE`, `CREATE`, `EXECUTE`, `ALL`. A
privilege that does not apply to an object is an error. `ALL` stands for every privilege of the
object's kind: tables, views and materialized views `SELECT` ... `MAINTAIN` (`MAINTAIN` is left out
on servers before 17), columns `SELECT, INSERT, UPDATE, REFERENCES`, sequences `USAGE, SELECT,
UPDATE`, schemas `USAGE, CREATE`, routines `EXECUTE`, types `USAGE`. `columns` (grants only) names
columns of tables, views or materialized views. Grantees are role references, role names and
`PUBLIC`; roles that are named but not declared are created (`NOLOGIN`) in scratch databases only.

**Semantics.** For every object, the desired privileges are PostgreSQL's built-in defaults for that
kind of object (for example `EXECUTE` for `PUBLIC` on functions and procedures, `USAGE` for `PUBLIC`
on types), minus `revoke` blocks, plus `grant` blocks. The owner's own privileges are never compared.
Only *managed grantees* are compared: `PUBLIC`, roles declared in the schema, and every role when
`manage_roles = true` (minus excluded roles). Setting `public_defaults = "revoke"` starts from no
`PUBLIC` privileges instead of PostgreSQL's defaults. Revokes never remove what a `grant` block
grants; granting a privilege both with and without `with_grant_option` keeps the grant option.

### `data "schema.table"`

```hcl
data "app.statuses" {
  rows = [
    { id = 1, name = "active" },
    { id = 2, name = "disabled", note = null },
  ]
  key  = ["id"]          # default: the table's primary key
  mode = sync            # sync | insert; default from the `data` setting
}
```

The table must be declared; without a primary key `key` is required. Values are strings, numbers,
bools or `null`; `sql()` is not supported in rows. Every row has the key columns, and two rows with
the same key are an error, also across several `data` blocks of one table (which must agree on
`key` and `mode`). Rows are compared by key. `sync` makes the table contain exactly the declared rows (INSERT, UPDATE,
DELETE); `insert` only inserts missing keys. Columns not mentioned in any row keep their values.
Values are compared in the database's own text form, so `1`, `1.0` and `"1"` written for a numeric
column are equal.

---

## ClickHouse

Supported servers: ClickHouse 24.8 and later (tested on 26.x). Default database: `default`.

### `database "name"`

| Attribute | Type | Notes |
|---|---|---|
| `name` | string | |
| `engine` | `Atomic` (default) or call | e.g. `Replicated("/clickhouse/databases/dialo", "{shard}", "{replica}")`. |
| `comment` | string | |

### `table "database.name"`

| Attribute / block | Type | Notes |
|---|---|---|
| `engine` | identifier or call | Required. `MergeTree`, `ReplacingMergeTree(updated_at)`, `ReplicatedMergeTree("/clickhouse/tables/{uuid}/{shard}", "{replica}")`, `TimeSeries`, `Memory`, ... |
| `column "name" {}` | block, repeated | |
| `order_by` | list of column refs/strings, or string | `ORDER BY`. A string is a raw expression (`"tuple()"`). |
| `primary_key` | list of column refs/strings, or string | Defaults to `order_by`. |
| `partition_by` | string | `PARTITION BY`. |
| `sample_by` | string | P1 |
| `ttl` | string | Table TTL expression(s). |
| `settings` | object | `SETTINGS k = v`. Values are numbers or strings. |
| `index "name" {}` | block | Data-skipping index: `expr` (string, required), `type` (identifier or call, required: `minmax`, `set(1024)`, `bloom_filter(0.01)`, `tokenbf_v1(32768, 3, 0)`, `ngrambf_v1(...)`), `granularity` (number, default 1). |
| `projection "name" { query = "..." }` | block | P1 |
| `constraint "name" { check = "..." }` or `{ assume = "..." }` | block | P1 |
| `comment`, `renamed_from`, `depends_on` | | |

Tables with `engine = TimeSeries` declare no columns.

#### `column "name"`

| Attribute | Type | Notes |
|---|---|---|
| `type` | type | Required. Nullability is part of the type (`Nullable(String)`). |
| `default` / `materialized` / `alias` / `ephemeral` | literal, `sql()` or string | At most one. `materialized`, `alias` take SQL strings; `default` takes a literal or `sql()`. |
| `codec` | list of identifiers/calls | `[Delta(8), ZSTD(1)]`. |
| `ttl` | string | Column TTL. |
| `comment`, `renamed_from` | | |

### `materialized_view "database.name"`

| Attribute | Type | Notes |
|---|---|---|
| `to` | table ref | Required in P0 (`TO` target table). |
| `as` | string | Required. The SELECT. |
| `comment`, `depends_on` | | |

P1: `engine` + `order_by` (inner table), `refresh = "EVERY 1 HOUR"`. `view "database.name"` (P1):
`as`, `comment`. `dictionary` and SQL `function` blocks are P1.

### `role "name"`

`storage` (`local_directory`, `replicated`; where the role is stored), `comment`.

### `user "name"` (P1)

`host`, `default_roles`, `settings`. No secrets.

### `grant`

```hcl
grant {
  privileges = [SELECT, INSERT]        # or [ALL]
  on         = [table.dialo.events, database.otel]   # or ["*.*"]
  columns    = ["call_id"]             # optional
  to         = [role.gateway]
  with_grant_option = true
}
```

ClickHouse has no default privileges. Privilege sets are compared after collapsing them with the
server's privilege hierarchy (`system.privileges`), so `ALL` matches the expanded list the server
reports.

### Cluster behaviour

With `cluster = "<name or {macro}>"` in `openmig.toml`, DDL for databases, roles, users and grants,
and for tables in non-`Replicated` databases, gets `ON CLUSTER`. Tables inside a `Replicated`
database never get `ON CLUSTER`: the database engine replicates them.
