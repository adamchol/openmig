//! Views and materialized views.

use super::{query_text, with_options};
use crate::model::{MaterializedView, QualifiedName, View, ViewCheckOption};
use std::collections::BTreeMap;

/// The reloptions a view's flags are stored as.
pub(crate) fn view_options(v: &View) -> BTreeMap<String, String> {
    let mut o = BTreeMap::new();
    if v.security_barrier {
        o.insert("security_barrier".to_owned(), "true".to_owned());
    }
    if v.security_invoker {
        o.insert("security_invoker".to_owned(), "true".to_owned());
    }
    if let Some(c) = v.check_option {
        o.insert(
            "check_option".to_owned(),
            match c {
                ViewCheckOption::Local => "local",
                ViewCheckOption::Cascaded => "cascaded",
            }
            .to_owned(),
        );
    }
    o
}

/// `CREATE [OR REPLACE] VIEW`. Replacing a view also replaces its options.
pub(crate) fn create_view(name: &QualifiedName, v: &View, or_replace: bool) -> String {
    let mut options = view_options(v);
    let check = options.remove("check_option");
    let mut sql = format!(
        "CREATE {}VIEW {}{} AS {}",
        if or_replace { "OR REPLACE " } else { "" },
        name.to_sql(),
        with_options(&options),
        query_text(&v.query).trim_start()
    );
    if let Some(c) = check {
        sql.push_str(&format!(" WITH {} CHECK OPTION", c.to_ascii_uppercase()));
    }
    sql
}

pub(crate) fn drop_view(name: &QualifiedName) -> String {
    format!("DROP VIEW {}", name.to_sql())
}

pub(crate) fn alter_view(name: &QualifiedName, action: &str) -> String {
    format!("ALTER VIEW {} {action}", name.to_sql())
}

pub(crate) fn create_materialized_view(name: &QualifiedName, m: &MaterializedView) -> String {
    format!(
        "CREATE MATERIALIZED VIEW {}{} AS {} WITH {}DATA",
        name.to_sql(),
        with_options(&m.storage_params),
        query_text(&m.query).trim_start(),
        if m.with_data { "" } else { "NO " }
    )
}

pub(crate) fn drop_materialized_view(name: &QualifiedName) -> String {
    format!("DROP MATERIALIZED VIEW {}", name.to_sql())
}

pub(crate) fn alter_materialized_view(name: &QualifiedName, action: &str) -> String {
    format!("ALTER MATERIALIZED VIEW {} {action}", name.to_sql())
}

pub(crate) fn refresh_materialized_view(name: &QualifiedName, with_data: bool) -> String {
    format!(
        "REFRESH MATERIALIZED VIEW {}{}",
        name.to_sql(),
        if with_data { "" } else { " WITH NO DATA" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views() {
        let v = View {
            name: "v".into(),
            query: " SELECT id\n   FROM app.orgs;".into(),
            security_barrier: true,
            check_option: Some(ViewCheckOption::Cascaded),
            ..View::default()
        };
        let name = QualifiedName::new("app", "v");
        assert_eq!(
            create_view(&name, &v, true),
            "CREATE OR REPLACE VIEW \"app\".\"v\" WITH (security_barrier = true) AS SELECT id\n   \
             FROM app.orgs WITH CASCADED CHECK OPTION"
        );
        let m = MaterializedView {
            name: "m".into(),
            query: " SELECT 1;".into(),
            with_data: false,
            ..MaterializedView::default()
        };
        assert_eq!(
            create_materialized_view(&QualifiedName::new("app", "m"), &m),
            "CREATE MATERIALIZED VIEW \"app\".\"m\" AS SELECT 1 WITH NO DATA"
        );
    }
}
