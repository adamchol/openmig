use crate::object::ObjectRef;

/// Whether a statement may run inside a transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TxRequirement {
    #[default]
    Any,
    /// Must run outside a transaction (`CREATE INDEX CONCURRENTLY`, using a just-added enum
    /// value). A plan containing one becomes a `no-transaction` migration.
    Forbidden,
}

/// One SQL statement of a plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    /// The statement without a trailing `;`.
    pub sql: String,
    /// One-line description written above the statement in migration files:
    /// `Create table "app"."users"`.
    pub comment: String,
    pub tx: TxRequirement,
    /// Why the statement destroys data, when it does (`drops table app.users and its rows`).
    pub destructive: Option<String>,
    /// The object the statement was generated for; used to point errors at the HCL declaration.
    pub origin: Option<ObjectRef>,
}

impl Statement {
    pub fn new(sql: impl Into<String>, comment: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            comment: comment.into(),
            tx: TxRequirement::Any,
            destructive: None,
            origin: None,
        }
    }

    pub fn no_transaction(mut self) -> Self {
        self.tx = TxRequirement::Forbidden;
        self
    }

    pub fn destructive(mut self, reason: impl Into<String>) -> Self {
        self.destructive = Some(reason.into());
        self
    }

    pub fn origin(mut self, origin: ObjectRef) -> Self {
        self.origin = Some(origin);
        self
    }
}

/// An ordered list of statements that turns one state into another.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub statements: Vec<Statement>,
    /// Things the user should know that are not errors (e.g. ignored owner differences).
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    pub fn len(&self) -> usize {
        self.statements.len()
    }

    /// True when at least one statement cannot run inside a transaction.
    pub fn requires_no_transaction(&self) -> bool {
        self.statements
            .iter()
            .any(|s| s.tx == TxRequirement::Forbidden)
    }

    pub fn destructive(&self) -> impl Iterator<Item = &Statement> {
        self.statements.iter().filter(|s| s.destructive.is_some())
    }

    pub fn push(&mut self, statement: Statement) {
        self.statements.push(statement);
    }

    pub fn extend(&mut self, other: Plan) {
        self.statements.extend(other.statements);
        self.warnings.extend(other.warnings);
    }

    /// The plan as SQL text: each statement preceded by its `-- comment` and terminated by `;`.
    pub fn to_sql(&self) -> String {
        let mut out = String::new();
        for s in &self.statements {
            if !s.comment.is_empty() {
                out.push_str("-- ");
                out.push_str(&s.comment);
                out.push('\n');
            }
            out.push_str(&s.sql);
            out.push_str(";\n");
        }
        out
    }
}
