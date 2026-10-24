//! `GRANT` and `REVOKE` of object and column privileges.

use super::{ident, role_spec};
use crate::model::Privilege;

fn privilege_list(privileges: &[Privilege], column: Option<&str>) -> String {
    let column = column
        .map(|c| format!(" ({})", ident(c)))
        .unwrap_or_default();
    privileges
        .iter()
        .map(|p| format!("{}{column}", p.as_sql()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `GRANT <privileges> [(<column>)] ON <target> TO <grantee> [WITH GRANT OPTION]`. `target` is
/// what follows `ON`: `TABLE "app"."users"`, `FUNCTION "app"."f"(integer)`.
pub(crate) fn grant(
    target: &str,
    column: Option<&str>,
    privileges: &[Privilege],
    grantee: &str,
    grant_option: bool,
) -> String {
    format!(
        "GRANT {} ON {target} TO {}{}",
        privilege_list(privileges, column),
        role_spec(grantee),
        if grant_option {
            " WITH GRANT OPTION"
        } else {
            ""
        }
    )
}

pub(crate) fn revoke(
    target: &str,
    column: Option<&str>,
    privileges: &[Privilege],
    grantee: &str,
) -> String {
    format!(
        "REVOKE {} ON {target} FROM {}",
        privilege_list(privileges, column),
        role_spec(grantee)
    )
}

pub(crate) fn revoke_grant_option(
    target: &str,
    column: Option<&str>,
    privileges: &[Privilege],
    grantee: &str,
) -> String {
    format!(
        "REVOKE GRANT OPTION FOR {} ON {target} FROM {}",
        privilege_list(privileges, column),
        role_spec(grantee)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grants() {
        assert_eq!(
            grant(
                "TABLE \"app\".\"t\"",
                None,
                &[Privilege::Select, Privilege::Insert],
                "web",
                true
            ),
            "GRANT SELECT, INSERT ON TABLE \"app\".\"t\" TO \"web\" WITH GRANT OPTION"
        );
        assert_eq!(
            revoke(
                "TABLE \"app\".\"t\"",
                Some("c"),
                &[Privilege::Update],
                "PUBLIC"
            ),
            "REVOKE UPDATE (\"c\") ON TABLE \"app\".\"t\" FROM PUBLIC"
        );
        assert_eq!(
            revoke_grant_option("SCHEMA \"app\"", None, &[Privilege::Create], "w"),
            "REVOKE GRANT OPTION FOR CREATE ON SCHEMA \"app\" FROM \"w\""
        );
    }
}
