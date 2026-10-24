use super::Acl;
use std::collections::BTreeMap;

/// A function or procedure. Identity is `(schema, name, input argument types)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    /// Arguments in declaration order, except `RETURNS TABLE` columns.
    pub args: Vec<FunctionArg>,
    /// `None` for procedures.
    pub returns: Option<Returns>,
    pub language: String,
    pub body: FunctionBody,
    pub volatility: Volatility,
    /// `STRICT` / `RETURNS NULL ON NULL INPUT`.
    pub strict: bool,
    pub security_definer: bool,
    pub leakproof: bool,
    pub parallel: Parallel,
    /// `COST`, as PostgreSQL prints the `real` value: `100`, `0.5`.
    pub cost: String,
    /// `ROWS` estimate for set-returning functions, `0` otherwise.
    pub rows: String,
    /// `WINDOW` function.
    pub window: bool,
    /// `SET name = value` clauses (`proconfig`), by parameter name, values as stored.
    pub config: BTreeMap<String, String>,
    pub comment: Option<String>,
    /// Recorded for information; owners are not managed.
    pub owner: String,
    pub acl: Acl,
}

impl Function {
    /// The key of this function within its schema: `name(integer, text)`, the name followed by
    /// the types of the `IN`, `INOUT` and `VARIADIC` arguments as `oidvectortypes` prints them.
    pub fn signature(&self) -> String {
        function_signature(&self.name, self.input_types())
    }

    /// Types of the arguments that are part of the identity.
    pub fn input_types(&self) -> impl Iterator<Item = &str> {
        self.args
            .iter()
            .filter(|a| a.mode.is_input())
            .map(|a| a.data_type.as_str())
    }
}

/// The key of a function within its schema: `name(type, type)`, see [`Function::signature`].
pub fn function_signature<'a>(
    name: &str,
    input_types: impl IntoIterator<Item = &'a str>,
) -> String {
    let types: Vec<&str> = input_types.into_iter().collect();
    format!("{name}({})", types.join(", "))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionArg {
    pub name: Option<String>,
    pub mode: ArgMode,
    /// `format_type(type, NULL)`; for `VARIADIC` the array type (`text[]`).
    pub data_type: String,
    /// Default expression, as `pg_get_function_arg_default` prints it.
    pub default: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArgMode {
    #[default]
    In,
    Out,
    InOut,
    Variadic,
}

impl ArgMode {
    pub fn as_sql(self) -> &'static str {
        match self {
            ArgMode::In => "IN",
            ArgMode::Out => "OUT",
            ArgMode::InOut => "INOUT",
            ArgMode::Variadic => "VARIADIC",
        }
    }

    /// Whether arguments of this mode are part of the function's identity.
    pub fn is_input(self) -> bool {
        !matches!(self, ArgMode::Out)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Returns {
    /// `RETURNS [SETOF] type`. Functions with `OUT` arguments return `record` (several) or the
    /// single `OUT` argument's type.
    Type { data_type: String, setof: bool },
    /// `RETURNS TABLE (...)`.
    Table(Vec<ReturnColumn>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReturnColumn {
    pub name: String,
    pub data_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FunctionBody {
    /// `AS '...'`: the source text (`prosrc`).
    Definition(String),
    /// A SQL-standard body (`BEGIN ATOMIC ... END` or `RETURN ...`), as
    /// `pg_get_function_sqlbody` prints it. Such bodies are parsed at creation and depend on the
    /// objects they use.
    Standard(String),
}

impl FunctionBody {
    pub fn text(&self) -> &str {
        match self {
            FunctionBody::Definition(s) | FunctionBody::Standard(s) => s,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Volatility {
    #[default]
    Volatile,
    Stable,
    Immutable,
}

impl Volatility {
    pub fn as_sql(self) -> &'static str {
        match self {
            Volatility::Volatile => "VOLATILE",
            Volatility::Stable => "STABLE",
            Volatility::Immutable => "IMMUTABLE",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Parallel {
    #[default]
    Unsafe,
    Restricted,
    Safe,
}

impl Parallel {
    pub fn as_sql(self) -> &'static str {
        match self {
            Parallel::Unsafe => "UNSAFE",
            Parallel::Restricted => "RESTRICTED",
            Parallel::Safe => "SAFE",
        }
    }
}
