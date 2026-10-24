//! Schemas, extensions, roles and event triggers.

use super::{ident, literal};
use crate::model::{EventTrigger, Membership, Role, TriggerEnabled};

pub(crate) fn create_schema(name: &str) -> String {
    format!("CREATE SCHEMA {}", ident(name))
}

pub(crate) fn drop_schema(name: &str) -> String {
    format!("DROP SCHEMA {}", ident(name))
}

pub(crate) fn create_extension(name: &str, schema: &str, version: Option<&str>) -> String {
    let mut sql = format!(
        "CREATE EXTENSION IF NOT EXISTS {} WITH SCHEMA {}",
        ident(name),
        ident(schema)
    );
    if let Some(v) = version {
        sql.push_str(&format!(" VERSION {}", literal(v)));
    }
    sql
}

pub(crate) fn alter_extension_schema(name: &str, schema: &str) -> String {
    format!(
        "ALTER EXTENSION {} SET SCHEMA {}",
        ident(name),
        ident(schema)
    )
}

pub(crate) fn drop_extension(name: &str) -> String {
    format!("DROP EXTENSION {}", ident(name))
}

fn flag(on: bool, word: &str) -> String {
    if on {
        word.to_owned()
    } else {
        format!("NO{word}")
    }
}

/// Role attributes that differ from `base` (all non-default ones when `base` is `None`).
fn role_options(role: &Role, base: Option<&Role>) -> Vec<String> {
    let default = Role::new(&role.name);
    let base = base.unwrap_or(&default);
    let mut out = Vec::new();
    for (on, was, word) in [
        (role.superuser, base.superuser, "SUPERUSER"),
        (role.createdb, base.createdb, "CREATEDB"),
        (role.createrole, base.createrole, "CREATEROLE"),
        (role.inherit, base.inherit, "INHERIT"),
        (role.login, base.login, "LOGIN"),
        (role.replication, base.replication, "REPLICATION"),
        (role.bypassrls, base.bypassrls, "BYPASSRLS"),
    ] {
        if on != was {
            out.push(flag(on, word));
        }
    }
    if role.connection_limit != base.connection_limit {
        out.push(format!("CONNECTION LIMIT {}", role.connection_limit));
    }
    if role.valid_until != base.valid_until {
        // A role's expiry cannot be reset to NULL; `infinity` is the closest equivalent.
        let until = role.valid_until.as_deref().unwrap_or("infinity");
        out.push(format!("VALID UNTIL {}", literal(until)));
    }
    out
}

pub(crate) fn create_role(role: &Role) -> String {
    let options = role_options(role, None);
    if options.is_empty() {
        format!("CREATE ROLE {}", ident(&role.name))
    } else {
        format!(
            "CREATE ROLE {} WITH {}",
            ident(&role.name),
            options.join(" ")
        )
    }
}

/// `CREATE ROLE ... NOLOGIN` unless a role of that name exists.
pub(crate) fn create_role_if_missing(name: &str) -> String {
    let body = format!(
        "BEGIN IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = {}) THEN CREATE ROLE {} NOLOGIN; END IF; END",
        literal(name),
        ident(name)
    );
    let tag = openmig_core::sql::dollar_tag(&body);
    format!("DO {tag}{body}{tag}")
}

pub(crate) fn alter_role(current: &Role, desired: &Role) -> Option<String> {
    let options = role_options(desired, Some(current));
    (!options.is_empty()).then(|| {
        format!(
            "ALTER ROLE {} WITH {}",
            ident(&desired.name),
            options.join(" ")
        )
    })
}

pub(crate) fn drop_role(name: &str) -> String {
    format!("DROP ROLE {}", ident(name))
}

/// `GRANT role TO member` with its options. Before PostgreSQL 16 only `ADMIN OPTION` exists.
pub(crate) fn grant_membership(role: &str, member: &str, m: &Membership, major: u32) -> String {
    let mut sql = format!("GRANT {} TO {}", ident(role), ident(member));
    if major >= 16 {
        sql.push_str(&format!(
            " WITH ADMIN {}, INHERIT {}, SET {}",
            bool_word(m.admin),
            bool_word(m.inherit),
            bool_word(m.set)
        ));
    } else if m.admin {
        sql.push_str(" WITH ADMIN OPTION");
    }
    sql
}

fn bool_word(b: bool) -> &'static str {
    if b { "TRUE" } else { "FALSE" }
}

/// `REVOKE [<option> OPTION FOR] role FROM member`.
pub(crate) fn revoke_membership(role: &str, member: &str, option: Option<&str>) -> String {
    let option = option
        .map(|o| format!("{o} OPTION FOR "))
        .unwrap_or_default();
    format!("REVOKE {option}{} FROM {}", ident(role), ident(member))
}

pub(crate) fn create_event_trigger(t: &EventTrigger) -> String {
    let mut sql = format!("CREATE EVENT TRIGGER {} ON {}", ident(&t.name), t.event);
    if !t.tags.is_empty() {
        let tags: Vec<String> = t.tags.iter().map(|x| literal(x)).collect();
        sql.push_str(&format!(" WHEN TAG IN ({})", tags.join(", ")));
    }
    sql.push_str(&format!(" EXECUTE FUNCTION {}()", t.function.to_sql()));
    sql
}

pub(crate) fn event_trigger_enabled(name: &str, enabled: TriggerEnabled) -> String {
    let action = match enabled {
        TriggerEnabled::Origin => "ENABLE",
        TriggerEnabled::Disabled => "DISABLE",
        TriggerEnabled::Replica => "ENABLE REPLICA",
        TriggerEnabled::Always => "ENABLE ALWAYS",
    };
    format!("ALTER EVENT TRIGGER {} {action}", ident(name))
}

pub(crate) fn drop_event_trigger(name: &str) -> String {
    format!("DROP EVENT TRIGGER {}", ident(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::QualifiedName;

    #[test]
    fn roles() {
        let mut r = Role::new("web");
        r.login = true;
        r.inherit = false;
        r.connection_limit = 10;
        r.valid_until = Some("2030-01-01 00:00:00+00".into());
        assert_eq!(
            create_role(&r),
            "CREATE ROLE \"web\" WITH NOINHERIT LOGIN CONNECTION LIMIT 10 VALID UNTIL '2030-01-01 \
             00:00:00+00'"
        );
        let mut s = r.clone();
        s.login = false;
        s.valid_until = None;
        assert_eq!(
            alter_role(&r, &s).unwrap(),
            "ALTER ROLE \"web\" WITH NOLOGIN VALID UNTIL 'infinity'"
        );
        assert_eq!(alter_role(&r, &r), None);
        let m = Membership {
            admin: true,
            inherit: true,
            set: false,
        };
        assert_eq!(
            grant_membership("admin", "web", &m, 16),
            "GRANT \"admin\" TO \"web\" WITH ADMIN TRUE, INHERIT TRUE, SET FALSE"
        );
        assert_eq!(
            grant_membership("admin", "web", &m, 15),
            "GRANT \"admin\" TO \"web\" WITH ADMIN OPTION"
        );
        assert_eq!(
            revoke_membership("admin", "web", Some("ADMIN")),
            "REVOKE ADMIN OPTION FOR \"admin\" FROM \"web\""
        );
        assert!(create_role_if_missing("x'y").contains("rolname = 'x''y'"));
    }

    #[test]
    fn event_triggers() {
        let t = EventTrigger {
            name: "ddl".into(),
            event: "ddl_command_end".into(),
            tags: vec!["ALTER TABLE".into(), "CREATE TABLE".into()],
            function: QualifiedName::new("app", "on_ddl"),
            enabled: TriggerEnabled::Origin,
            comment: None,
            owner: String::new(),
        };
        assert_eq!(
            create_event_trigger(&t),
            "CREATE EVENT TRIGGER \"ddl\" ON ddl_command_end WHEN TAG IN ('ALTER TABLE', 'CREATE \
             TABLE') EXECUTE FUNCTION \"app\".\"on_ddl\"()"
        );
    }
}
