//! Functions and procedures.

use super::{AlterOp, Differ, merge};
use crate::model::{Function, Schema};
use openmig_core::{ObjectKind, ObjectRef};

/// Whether `CREATE OR REPLACE` can turn `cur` into `des`: same kind, return type, arguments
/// (modes, types and names, so also `OUT` parameters) and no argument default removed.
pub(crate) fn replaceable(cur: &Function, des: &Function) -> bool {
    cur.returns == des.returns
        && cur.window == des.window
        && cur.args.len() == des.args.len()
        && cur.args.iter().zip(&des.args).all(|(c, d)| {
            c.mode == d.mode
                && c.data_type == d.data_type
                && c.name == d.name
                && (c.default.is_none() || d.default.is_some())
        })
}

/// Whether only attributes that `ALTER FUNCTION` can change differ.
fn attributes_only(cur: &Function, des: &Function) -> bool {
    cur.language == des.language
        && cur.body == des.body
        && cur.args == des.args
        && cur.returns == des.returns
        && cur.window == des.window
}

/// Whether the definitions differ, ignoring owner, privileges and comment.
fn definition_differs(cur: &Function, des: &Function) -> bool {
    let strip = |f: &Function| Function {
        comment: None,
        owner: String::new(),
        acl: Default::default(),
        ..f.clone()
    };
    strip(cur) != strip(des)
}

impl Differ<'_> {
    pub(super) fn routines(&mut self, cur: &Schema, des: &Schema, kind: ObjectKind) {
        let s = if des.name.is_empty() {
            &cur.name
        } else {
            &des.name
        };
        let (cur_map, des_map) = if kind == ObjectKind::Function {
            (&cur.functions, &des.functions)
        } else {
            (&cur.procedures, &des.procedures)
        };
        for (sig, c, d) in merge(cur_map, des_map) {
            let r = ObjectRef::new(kind, [s, sig]);
            let (c, d) = match (c, d) {
                (None, Some(_)) => {
                    self.create(r);
                    continue;
                }
                (Some(_), None) => {
                    self.drop(r);
                    continue;
                }
                (Some(c), Some(d)) => (c, d),
                (None, None) => continue,
            };
            let mut ops = Vec::new();
            if definition_differs(c, d) {
                if !replaceable(c, d) {
                    self.recreate(r, "the signature, return type or arguments changed");
                    continue;
                }
                ops.push(if attributes_only(c, d) {
                    AlterOp::Attributes
                } else {
                    AlterOp::Replace
                });
            }
            Self::comment_op(&mut ops, &c.comment, &d.comment);
            self.acl_op(&mut ops, &c.acl, &d.acl);
            self.alter(r, ops);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ArgMode, FunctionArg, FunctionBody, Parallel, Returns, Volatility};

    fn f() -> Function {
        Function {
            name: "f".into(),
            args: vec![FunctionArg {
                name: Some("a".into()),
                mode: ArgMode::In,
                data_type: "integer".into(),
                default: Some("1".into()),
            }],
            returns: Some(Returns::Type {
                data_type: "integer".into(),
                setof: false,
            }),
            language: "sql".into(),
            body: FunctionBody::Definition("SELECT a".into()),
            volatility: Volatility::Volatile,
            strict: false,
            security_definer: false,
            leakproof: false,
            parallel: Parallel::Unsafe,
            cost: "100".into(),
            rows: "0".into(),
            window: false,
            config: Default::default(),
            comment: None,
            owner: String::new(),
            acl: Default::default(),
        }
    }

    #[test]
    fn replace_rules() {
        let a = f();
        let mut b = f();
        b.body = FunctionBody::Definition("SELECT a + 1".into());
        assert!(replaceable(&a, &b));
        assert!(!attributes_only(&a, &b));
        b.args[0].name = Some("x".into());
        assert!(!replaceable(&a, &b));
        let mut b = f();
        b.args[0].default = None;
        assert!(!replaceable(&a, &b));
        assert!(replaceable(&b, &a));
        let mut b = f();
        b.returns = Some(Returns::Type {
            data_type: "bigint".into(),
            setof: false,
        });
        assert!(!replaceable(&a, &b));
        let mut b = f();
        b.volatility = Volatility::Stable;
        assert!(attributes_only(&a, &b));
        assert!(definition_differs(&a, &b));
        let mut b = f();
        b.comment = Some("x".into());
        assert!(!definition_differs(&a, &b));
    }
}
