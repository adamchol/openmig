//! Tables, columns, constraints, indexes, policies and triggers.

use super::{ident, ident_list, literal, option_list, qualified, role_spec, with_options};
use crate::model::{
    Check, Column, Exclusion, FkAction, FkMatch, ForeignKey, GeneratedKind, Identity,
    IdentityGeneration, Index, IndexKey, KeyExpr, NullsOrder, Policy, PrimaryKey, QualifiedName,
    ReplicaIdentity, SequenceOptions, Table, Trigger, TriggerEnabled, TriggerLevel, Unique,
};
use std::collections::BTreeMap;

/// `CREATE TABLE` with the given columns and inline constraints (primary key, unique, check and
/// exclusion constraints that are valid, plus `fks`). Partitions name their parent instead of
/// listing columns.
pub(crate) fn create_table(
    name: &QualifiedName,
    t: &Table,
    columns: &[&Column],
    fks: &[&ForeignKey],
) -> String {
    let mut items: Vec<String> = Vec::new();
    if t.partition_of.is_none() {
        items.extend(columns.iter().map(|c| column_definition(c)));
    }
    if let Some(pk) = &t.primary_key {
        items.push(primary_key_definition(pk));
    }
    items.extend(t.uniques.values().map(unique_definition));
    items.extend(
        t.checks
            .values()
            .filter(|c| c.validated)
            .map(check_definition),
    );
    items.extend(t.exclusions.values().map(exclusion_definition));
    items.extend(fks.iter().map(|fk| foreign_key_definition(fk)));
    let unlogged = if t.unlogged { "UNLOGGED " } else { "" };
    let mut sql = format!("CREATE {unlogged}TABLE {}", name.to_sql());
    if let Some(p) = &t.partition_of {
        sql.push_str(&format!(" PARTITION OF {}", p.parent.to_sql()));
        if !items.is_empty() {
            sql.push_str(&format!(" ({})", items.join(", ")));
        }
        sql.push(' ');
        sql.push_str(&p.bound);
    } else {
        sql.push_str(&format!(" ({})", items.join(", ")));
        if !t.inherits.is_empty() {
            let parents: Vec<String> = t.inherits.iter().map(QualifiedName::to_sql).collect();
            sql.push_str(&format!(" INHERITS ({})", parents.join(", ")));
        }
    }
    if let Some(by) = &t.partition_by {
        sql.push_str(&format!(" PARTITION BY {by}"));
    }
    sql.push_str(&with_options(&t.storage_params));
    sql
}

/// A column as written in `CREATE TABLE` and `ADD COLUMN` (without storage, which needs
/// `ALTER COLUMN ... SET STORAGE`).
pub(crate) fn column_definition(c: &Column) -> String {
    let mut sql = format!("{} {}", ident(&c.name), c.data_type);
    if let Some(collation) = &c.collation {
        sql.push_str(&format!(" COLLATE {collation}"));
    }
    if let Some(compression) = &c.compression {
        sql.push_str(&format!(" COMPRESSION {compression}"));
    }
    if let Some(g) = &c.generated {
        let kind = match g.kind {
            GeneratedKind::Stored => "STORED",
            GeneratedKind::Virtual => "VIRTUAL",
        };
        sql.push_str(&format!(" GENERATED ALWAYS AS ({}) {kind}", g.expr));
    }
    if let Some(identity) = &c.identity {
        sql.push(' ');
        sql.push_str(&identity_clause(identity));
    }
    if let Some(default) = &c.default {
        sql.push_str(&format!(" DEFAULT {default}"));
    }
    if c.not_null {
        sql.push_str(" NOT NULL");
    }
    sql
}

/// `GENERATED ALWAYS AS IDENTITY (START WITH 1 ...)`.
pub(crate) fn identity_clause(identity: &Identity) -> String {
    format!(
        "GENERATED {} AS IDENTITY ({})",
        identity_generation(identity.generation),
        sequence_options(&identity.sequence)
    )
}

pub(crate) fn identity_generation(g: IdentityGeneration) -> &'static str {
    match g {
        IdentityGeneration::Always => "ALWAYS",
        IdentityGeneration::ByDefault => "BY DEFAULT",
    }
}

/// `START WITH 1 INCREMENT BY 1 MINVALUE 1 MAXVALUE 100 CACHE 1 NO CYCLE`.
pub(crate) fn sequence_options(o: &SequenceOptions) -> String {
    format!(
        "START WITH {} INCREMENT BY {} MINVALUE {} MAXVALUE {} CACHE {} {}",
        o.start,
        o.increment,
        o.min_value,
        o.max_value,
        o.cache,
        if o.cycle { "CYCLE" } else { "NO CYCLE" }
    )
}

/// Identity sequence options as `ALTER COLUMN` subcommands: `SET START WITH 1 SET INCREMENT BY 1 ...`.
pub(crate) fn identity_set_options(o: &SequenceOptions) -> String {
    format!(
        "SET START WITH {} SET INCREMENT BY {} SET MINVALUE {} SET MAXVALUE {} SET CACHE {} SET {}",
        o.start,
        o.increment,
        o.min_value,
        o.max_value,
        o.cache,
        if o.cycle { "CYCLE" } else { "NO CYCLE" }
    )
}

fn deferrable(deferrable: bool, initially_deferred: bool) -> &'static str {
    match (deferrable, initially_deferred) {
        (false, _) => "",
        (true, false) => " DEFERRABLE",
        (true, true) => " DEFERRABLE INITIALLY DEFERRED",
    }
}

fn include(columns: &[String]) -> String {
    if columns.is_empty() {
        String::new()
    } else {
        format!(" INCLUDE ({})", ident_list(columns))
    }
}

pub(crate) fn primary_key_definition(pk: &PrimaryKey) -> String {
    format!(
        "CONSTRAINT {} PRIMARY KEY ({}){}{}{}",
        ident(&pk.name),
        ident_list(&pk.columns),
        include(&pk.include),
        with_options(&pk.storage_params),
        deferrable(pk.deferrable, pk.initially_deferred)
    )
}

pub(crate) fn unique_definition(u: &Unique) -> String {
    format!(
        "CONSTRAINT {} UNIQUE{} ({}){}{}{}",
        ident(&u.name),
        if u.nulls_distinct {
            ""
        } else {
            " NULLS NOT DISTINCT"
        },
        ident_list(&u.columns),
        include(&u.include),
        with_options(&u.storage_params),
        deferrable(u.deferrable, u.initially_deferred)
    )
}

pub(crate) fn check_definition(c: &Check) -> String {
    format!(
        "CONSTRAINT {} CHECK ({}){}{}",
        ident(&c.name),
        c.expr,
        if c.no_inherit { " NO INHERIT" } else { "" },
        if c.enforced { "" } else { " NOT ENFORCED" }
    )
}

pub(crate) fn exclusion_definition(x: &Exclusion) -> String {
    let elements: Vec<String> = x
        .elements
        .iter()
        .map(|e| format!("{} WITH {}", index_key(&e.key), e.operator))
        .collect();
    let mut sql = format!(
        "CONSTRAINT {} EXCLUDE USING {} ({}){}{}",
        ident(&x.name),
        x.method,
        elements.join(", "),
        include(&x.include),
        with_options(&x.storage_params)
    );
    if let Some(p) = &x.predicate {
        sql.push_str(&format!(" WHERE ({p})"));
    }
    sql.push_str(deferrable(x.deferrable, x.initially_deferred));
    sql
}

pub(crate) fn foreign_key_definition(fk: &ForeignKey) -> String {
    let mut sql = format!(
        "CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
        ident(&fk.name),
        ident_list(&fk.columns),
        fk.ref_table.to_sql(),
        ident_list(&fk.ref_columns)
    );
    if fk.match_type != FkMatch::Simple {
        sql.push_str(&format!(" MATCH {}", fk.match_type.as_sql()));
    }
    if fk.on_update != FkAction::NoAction {
        sql.push_str(&format!(" ON UPDATE {}", fk.on_update.as_sql()));
    }
    if fk.on_delete != FkAction::NoAction {
        sql.push_str(&format!(" ON DELETE {}", fk.on_delete.as_sql()));
    }
    sql.push_str(deferrable(fk.deferrable, fk.initially_deferred));
    if !fk.enforced {
        sql.push_str(" NOT ENFORCED");
    }
    sql
}

/// `ALTER TABLE ... ADD <definition> [NOT VALID]`.
pub(crate) fn add_constraint(table: &QualifiedName, definition: &str, not_valid: bool) -> String {
    format!(
        "ALTER TABLE {} ADD {definition}{}",
        table.to_sql(),
        if not_valid { " NOT VALID" } else { "" }
    )
}

pub(crate) fn drop_constraint(table: &QualifiedName, name: &str) -> String {
    format!(
        "ALTER TABLE {} DROP CONSTRAINT {}",
        table.to_sql(),
        ident(name)
    )
}

pub(crate) fn validate_constraint(table: &QualifiedName, name: &str) -> String {
    format!(
        "ALTER TABLE {} VALIDATE CONSTRAINT {}",
        table.to_sql(),
        ident(name)
    )
}

pub(crate) fn add_column(table: &QualifiedName, c: &Column) -> String {
    format!(
        "ALTER TABLE {} ADD COLUMN {}",
        table.to_sql(),
        column_definition(c)
    )
}

pub(crate) fn drop_column(table: &QualifiedName, name: &str) -> String {
    format!("ALTER TABLE {} DROP COLUMN {}", table.to_sql(), ident(name))
}

/// `ALTER TABLE t ALTER COLUMN c <action>`.
pub(crate) fn alter_column(table: &QualifiedName, column: &str, action: &str) -> String {
    format!(
        "ALTER TABLE {} ALTER COLUMN {} {action}",
        table.to_sql(),
        ident(column)
    )
}

/// `TYPE <type> [COLLATE c] USING "c"::<type>`.
pub(crate) fn column_type_action(c: &Column) -> String {
    let collate = c
        .collation
        .as_ref()
        .map(|x| format!(" COLLATE {x}"))
        .unwrap_or_default();
    format!(
        "TYPE {}{collate} USING {}::{}",
        c.data_type,
        ident(&c.name),
        c.data_type
    )
}

pub(crate) fn alter_table(table: &QualifiedName, action: &str) -> String {
    format!("ALTER TABLE {} {action}", table.to_sql())
}

pub(crate) fn drop_table(table: &QualifiedName) -> String {
    format!("DROP TABLE {}", table.to_sql())
}

pub(crate) fn rename_table(table: &QualifiedName, to: &str) -> String {
    format!("ALTER TABLE {} RENAME TO {}", table.to_sql(), ident(to))
}

pub(crate) fn set_table_schema(table: &QualifiedName, schema: &str) -> String {
    format!(
        "ALTER TABLE {} SET SCHEMA {}",
        table.to_sql(),
        ident(schema)
    )
}

pub(crate) fn rename_column(table: &QualifiedName, from: &str, to: &str) -> String {
    format!(
        "ALTER TABLE {} RENAME COLUMN {} TO {}",
        table.to_sql(),
        ident(from),
        ident(to)
    )
}

pub(crate) fn replica_identity(r: &ReplicaIdentity) -> String {
    match r {
        ReplicaIdentity::Default => "REPLICA IDENTITY DEFAULT".into(),
        ReplicaIdentity::Full => "REPLICA IDENTITY FULL".into(),
        ReplicaIdentity::Nothing => "REPLICA IDENTITY NOTHING".into(),
        ReplicaIdentity::Index(i) => format!("REPLICA IDENTITY USING INDEX {}", ident(i)),
    }
}

/// `RESET (...)` for removed parameters and `SET (...)` for added or changed ones (empty when
/// nothing differs).
pub(crate) fn storage_param_actions(
    current: &BTreeMap<String, String>,
    desired: &BTreeMap<String, String>,
) -> Vec<String> {
    let set: BTreeMap<String, String> = desired
        .iter()
        .filter(|(k, v)| current.get(*k) != Some(v))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let reset: Vec<&String> = current
        .keys()
        .filter(|k| !desired.contains_key(*k))
        .collect();
    let mut out = Vec::new();
    if !reset.is_empty() {
        let keys: Vec<&str> = reset.iter().map(|k| k.as_str()).collect();
        out.push(format!("RESET ({})", keys.join(", ")));
    }
    if !set.is_empty() {
        out.push(format!("SET ({})", option_list(&set)));
    }
    out
}

pub(crate) fn index_key(k: &IndexKey) -> String {
    let mut sql = match &k.expr {
        KeyExpr::Column(c) => ident(c),
        KeyExpr::Expression(e) => format!("({e})"),
    };
    if let Some(c) = &k.collation {
        sql.push_str(&format!(" COLLATE {c}"));
    }
    if let Some(o) = &k.opclass {
        sql.push_str(&format!(" {o}"));
    }
    if k.descending {
        sql.push_str(" DESC");
    }
    match k.nulls {
        Some(NullsOrder::First) => sql.push_str(" NULLS FIRST"),
        Some(NullsOrder::Last) => sql.push_str(" NULLS LAST"),
        None => {}
    }
    sql
}

pub(crate) fn create_index(table: &QualifiedName, i: &Index, concurrently: bool) -> String {
    let keys: Vec<String> = i.keys.iter().map(index_key).collect();
    let mut sql = format!(
        "CREATE {}INDEX {}{} ON {} USING {} ({}){}",
        if i.unique { "UNIQUE " } else { "" },
        if concurrently { "CONCURRENTLY " } else { "" },
        ident(&i.name),
        table.to_sql(),
        i.method,
        keys.join(", "),
        include(&i.include)
    );
    if !i.nulls_distinct {
        sql.push_str(" NULLS NOT DISTINCT");
    }
    sql.push_str(&with_options(&i.storage_params));
    if let Some(p) = &i.predicate {
        sql.push_str(&format!(" WHERE ({p})"));
    }
    sql
}

pub(crate) fn drop_index(schema: &str, name: &str, concurrently: bool, if_exists: bool) -> String {
    format!(
        "DROP INDEX {}{}{}",
        if concurrently { "CONCURRENTLY " } else { "" },
        if if_exists { "IF EXISTS " } else { "" },
        qualified(schema, name)
    )
}

fn role_list(roles: &[String]) -> String {
    roles
        .iter()
        .map(|r| role_spec(r))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn create_policy(table: &QualifiedName, p: &Policy) -> String {
    let mut sql = format!(
        "CREATE POLICY {} ON {} AS {} FOR {}",
        ident(&p.name),
        table.to_sql(),
        if p.permissive {
            "PERMISSIVE"
        } else {
            "RESTRICTIVE"
        },
        p.command.as_sql()
    );
    if !p.roles.is_empty() {
        sql.push_str(&format!(" TO {}", role_list(&p.roles)));
    }
    if let Some(u) = &p.using {
        sql.push_str(&format!(" USING ({u})"));
    }
    if let Some(c) = &p.check {
        sql.push_str(&format!(" WITH CHECK ({c})"));
    }
    sql
}

/// `ALTER POLICY` for the roles, `USING` and `WITH CHECK` parts that differ. Removing a `USING`
/// or `WITH CHECK` clause is not possible this way; callers recreate the policy instead.
pub(crate) fn alter_policy(table: &QualifiedName, current: &Policy, desired: &Policy) -> String {
    let mut sql = format!(
        "ALTER POLICY {} ON {}",
        ident(&desired.name),
        table.to_sql()
    );
    if current.roles != desired.roles && !desired.roles.is_empty() {
        sql.push_str(&format!(" TO {}", role_list(&desired.roles)));
    }
    if current.using != desired.using
        && let Some(u) = &desired.using
    {
        sql.push_str(&format!(" USING ({u})"));
    }
    if current.check != desired.check
        && let Some(c) = &desired.check
    {
        sql.push_str(&format!(" WITH CHECK ({c})"));
    }
    sql
}

pub(crate) fn drop_policy(table: &QualifiedName, name: &str) -> String {
    format!("DROP POLICY {} ON {}", ident(name), table.to_sql())
}

pub(crate) fn create_trigger(table: &QualifiedName, t: &Trigger, or_replace: bool) -> String {
    let events: Vec<String> = t
        .events
        .iter()
        .map(|e| {
            if *e == crate::model::TriggerEvent::Update && !t.update_of.is_empty() {
                format!("UPDATE OF {}", ident_list(&t.update_of))
            } else {
                e.as_sql().to_owned()
            }
        })
        .collect();
    let mut sql = format!(
        "CREATE {}{}TRIGGER {} {} {} ON {}",
        if or_replace { "OR REPLACE " } else { "" },
        if t.constraint.is_some() {
            "CONSTRAINT "
        } else {
            ""
        },
        ident(&t.name),
        t.timing.as_sql(),
        events.join(" OR "),
        table.to_sql()
    );
    if let Some(c) = &t.constraint {
        if let Some(from) = &c.from {
            sql.push_str(&format!(" FROM {}", from.to_sql()));
        }
        sql.push_str(match (c.deferrable, c.initially_deferred) {
            (false, _) => " NOT DEFERRABLE",
            (true, false) => " DEFERRABLE INITIALLY IMMEDIATE",
            (true, true) => " DEFERRABLE INITIALLY DEFERRED",
        });
    }
    if t.old_table.is_some() || t.new_table.is_some() {
        sql.push_str(" REFERENCING");
        if let Some(o) = &t.old_table {
            sql.push_str(&format!(" OLD TABLE AS {}", ident(o)));
        }
        if let Some(n) = &t.new_table {
            sql.push_str(&format!(" NEW TABLE AS {}", ident(n)));
        }
    }
    sql.push_str(match t.level {
        TriggerLevel::Row => " FOR EACH ROW",
        TriggerLevel::Statement => " FOR EACH STATEMENT",
    });
    if let Some(w) = &t.when {
        sql.push_str(&format!(" WHEN ({w})"));
    }
    let args: Vec<String> = t.args.iter().map(|a| literal(a)).collect();
    sql.push_str(&format!(
        " EXECUTE FUNCTION {}({})",
        t.function.to_sql(),
        args.join(", ")
    ));
    sql
}

pub(crate) fn drop_trigger(table: &QualifiedName, name: &str) -> String {
    format!("DROP TRIGGER {} ON {}", ident(name), table.to_sql())
}

/// `ENABLE TRIGGER`, `DISABLE TRIGGER`, `ENABLE REPLICA TRIGGER` or `ENABLE ALWAYS TRIGGER`.
pub(crate) fn trigger_enabled(
    table: &QualifiedName,
    name: &str,
    enabled: TriggerEnabled,
) -> String {
    let action = match enabled {
        TriggerEnabled::Origin => "ENABLE",
        TriggerEnabled::Disabled => "DISABLE",
        TriggerEnabled::Replica => "ENABLE REPLICA",
        TriggerEnabled::Always => "ENABLE ALWAYS",
    };
    format!(
        "ALTER TABLE {} {action} TRIGGER {}",
        table.to_sql(),
        ident(name)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        ConstraintTrigger, ExclusionElement, Generated, PartitionOf, PolicyCommand, TriggerEvent,
        TriggerTiming,
    };
    use std::collections::BTreeSet;

    fn users() -> QualifiedName {
        QualifiedName::new("app", "users")
    }

    #[test]
    fn create_table_with_everything_inline() {
        let mut t = Table::new("users");
        let mut id = Column::new("id", "bigint");
        id.not_null = true;
        id.identity = Some(Identity {
            generation: IdentityGeneration::Always,
            sequence: SequenceOptions::defaults_for("bigint"),
        });
        let mut name = Column::new("name", "text");
        name.collation = Some("\"C\"".into());
        name.default = Some("'x'::text".into());
        let mut full = Column::new("full", "text");
        full.generated = Some(Generated {
            expr: "name || 'y'".into(),
            kind: GeneratedKind::Stored,
        });
        t.columns = vec![id, name, full];
        t.primary_key = Some(PrimaryKey {
            name: "users_pkey".into(),
            columns: vec!["id".into()],
            ..PrimaryKey::default()
        });
        t.checks.insert(
            "c".into(),
            Check {
                name: "c".into(),
                expr: "length(name) > 0".into(),
                no_inherit: false,
                validated: true,
                enforced: true,
                comment: None,
            },
        );
        t.unlogged = true;
        t.storage_params.insert("fillfactor".into(), "70".into());
        let fk = ForeignKey {
            name: "users_org_fk".into(),
            columns: vec!["id".into()],
            ref_table: QualifiedName::new("app", "orgs"),
            ref_columns: vec!["id".into()],
            on_update: FkAction::NoAction,
            on_delete: FkAction::Cascade,
            match_type: FkMatch::Simple,
            deferrable: true,
            initially_deferred: true,
            validated: true,
            enforced: true,
            comment: None,
        };
        let cols: Vec<&Column> = t.columns.iter().collect();
        assert_eq!(
            create_table(&users(), &t, &cols, &[&fk]),
            "CREATE UNLOGGED TABLE \"app\".\"users\" (\"id\" bigint GENERATED ALWAYS AS IDENTITY \
             (START WITH 1 INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 CACHE 1 NO \
             CYCLE) NOT NULL, \"name\" text COLLATE \"C\" DEFAULT 'x'::text, \"full\" text \
             GENERATED ALWAYS AS (name || 'y') STORED, CONSTRAINT \"users_pkey\" PRIMARY KEY \
             (\"id\"), CONSTRAINT \"c\" CHECK (length(name) > 0), CONSTRAINT \"users_org_fk\" \
             FOREIGN KEY (\"id\") REFERENCES \"app\".\"orgs\" (\"id\") ON DELETE CASCADE \
             DEFERRABLE INITIALLY DEFERRED) WITH (fillfactor = 70)"
        );
    }

    #[test]
    fn partitions_and_inheritance() {
        let mut t = Table::new("events_2026");
        t.columns.push(Column::new("at", "timestamptz"));
        t.partition_of = Some(PartitionOf {
            parent: QualifiedName::new("app", "events"),
            bound: "FOR VALUES FROM ('2026-01-01') TO ('2027-01-01')".into(),
        });
        let cols: Vec<&Column> = t.columns.iter().collect();
        assert_eq!(
            create_table(&QualifiedName::new("app", "events_2026"), &t, &cols, &[]),
            "CREATE TABLE \"app\".\"events_2026\" PARTITION OF \"app\".\"events\" FOR VALUES \
             FROM ('2026-01-01') TO ('2027-01-01')"
        );
        let mut t = Table::new("login_log");
        t.columns.push(Column::new("ip", "inet"));
        t.inherits.push(QualifiedName::new("audit", "log"));
        t.partition_by = Some("RANGE (ip)".into());
        let cols: Vec<&Column> = t.columns.iter().collect();
        assert_eq!(
            create_table(&QualifiedName::new("audit", "login_log"), &t, &cols, &[]),
            "CREATE TABLE \"audit\".\"login_log\" (\"ip\" inet) INHERITS (\"audit\".\"log\") \
             PARTITION BY RANGE (ip)"
        );
    }

    #[test]
    fn constraints() {
        let u = Unique {
            name: "u".into(),
            columns: vec!["a".into(), "b".into()],
            include: vec!["c".into()],
            nulls_distinct: false,
            deferrable: true,
            initially_deferred: false,
            storage_params: [("fillfactor".to_owned(), "90".to_owned())].into(),
            comment: None,
        };
        assert_eq!(
            unique_definition(&u),
            "CONSTRAINT \"u\" UNIQUE NULLS NOT DISTINCT (\"a\", \"b\") INCLUDE (\"c\") WITH \
             (fillfactor = 90) DEFERRABLE"
        );
        let x = Exclusion {
            name: "x".into(),
            method: "gist".into(),
            elements: vec![
                ExclusionElement {
                    key: IndexKey::column("org_id"),
                    operator: "=".into(),
                },
                ExclusionElement {
                    key: IndexKey::column("during"),
                    operator: "&&".into(),
                },
            ],
            include: vec![],
            predicate: Some("id > 0".into()),
            deferrable: false,
            initially_deferred: false,
            storage_params: BTreeMap::new(),
            comment: None,
        };
        assert_eq!(
            exclusion_definition(&x),
            "CONSTRAINT \"x\" EXCLUDE USING gist (\"org_id\" WITH =, \"during\" WITH &&) WHERE \
             (id > 0)"
        );
        assert_eq!(
            add_constraint(&users(), "CONSTRAINT \"c\" CHECK (x)", true),
            "ALTER TABLE \"app\".\"users\" ADD CONSTRAINT \"c\" CHECK (x) NOT VALID"
        );
    }

    #[test]
    fn indexes() {
        let i = Index {
            name: "users_name_idx".into(),
            keys: vec![
                IndexKey {
                    expr: KeyExpr::Column("last_name".into()),
                    opclass: None,
                    collation: Some("\"C\"".into()),
                    descending: false,
                    nulls: None,
                },
                IndexKey {
                    expr: KeyExpr::Expression("lower(email)".into()),
                    opclass: Some("text_pattern_ops".into()),
                    collation: None,
                    descending: true,
                    nulls: Some(NullsOrder::Last),
                },
            ],
            include: vec!["id".into()],
            predicate: Some("(status <> 'deleted'::app.status)".into()),
            unique: true,
            nulls_distinct: false,
            ..Index::default()
        };
        assert_eq!(
            create_index(&users(), &i, true),
            "CREATE UNIQUE INDEX CONCURRENTLY \"users_name_idx\" ON \"app\".\"users\" USING btree \
             (\"last_name\" COLLATE \"C\", (lower(email)) text_pattern_ops DESC NULLS LAST) \
             INCLUDE (\"id\") NULLS NOT DISTINCT WHERE ((status <> 'deleted'::app.status))"
        );
        assert_eq!(
            drop_index("app", "i", true, true),
            "DROP INDEX CONCURRENTLY IF EXISTS \"app\".\"i\""
        );
    }

    #[test]
    fn policies() {
        let p = Policy {
            name: "p".into(),
            command: PolicyCommand::Select,
            permissive: false,
            roles: vec!["PUBLIC".into(), "web".into()],
            using: Some("true".into()),
            check: None,
            comment: None,
        };
        assert_eq!(
            create_policy(&users(), &p),
            "CREATE POLICY \"p\" ON \"app\".\"users\" AS RESTRICTIVE FOR SELECT TO PUBLIC, \"web\" \
             USING (true)"
        );
        let mut q = p.clone();
        q.roles = vec!["web".into()];
        q.check = Some("x".into());
        assert_eq!(
            alter_policy(&users(), &p, &q),
            "ALTER POLICY \"p\" ON \"app\".\"users\" TO \"web\" WITH CHECK (x)"
        );
    }

    #[test]
    fn triggers() {
        let t = Trigger {
            name: "t".into(),
            timing: TriggerTiming::After,
            events: BTreeSet::from([TriggerEvent::Insert, TriggerEvent::Update]),
            update_of: vec!["a".into()],
            level: TriggerLevel::Row,
            when: Some("old.* IS DISTINCT FROM new.*".into()),
            function: QualifiedName::new("app", "f"),
            args: vec!["x'y".into()],
            enabled: TriggerEnabled::Origin,
            constraint: Some(ConstraintTrigger {
                deferrable: true,
                initially_deferred: true,
                from: None,
            }),
            old_table: None,
            new_table: None,
            comment: None,
        };
        assert_eq!(
            create_trigger(&users(), &t, false),
            "CREATE CONSTRAINT TRIGGER \"t\" AFTER INSERT OR UPDATE OF \"a\" ON \"app\".\"users\" \
             DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN (old.* IS DISTINCT FROM new.*) \
             EXECUTE FUNCTION \"app\".\"f\"('x''y')"
        );
        assert_eq!(
            trigger_enabled(&users(), "t", TriggerEnabled::Always),
            "ALTER TABLE \"app\".\"users\" ENABLE ALWAYS TRIGGER \"t\""
        );
    }

    #[test]
    fn storage_params() {
        let cur = [
            ("a".to_owned(), "1".to_owned()),
            ("b".to_owned(), "2".to_owned()),
        ]
        .into();
        let des = [("a".to_owned(), "3".to_owned())].into();
        assert_eq!(
            storage_param_actions(&cur, &des),
            vec!["RESET (b)".to_owned(), "SET (a = 3)".to_owned()]
        );
    }
}
