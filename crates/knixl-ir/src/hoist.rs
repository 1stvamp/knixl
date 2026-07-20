//! let-hoisting: a generator pass (docs/03) that deduplicates repeated compound
//! subexpressions within one file into top-level `let` bindings.
//!
//! A value is hoisted when it appears verbatim twice or more in the file and is
//! compound (a non-empty attrset, a non-empty list, or an indented string). Equality
//! is emitted-text equality, so the pass agrees with what the lock hashes. Hoisting is
//! maximal: the largest repeated expression is bound and its interior is left literal,
//! so bindings never reference other bindings.

use std::collections::{BTreeMap, BTreeSet};

use crate::emit::{Emit, Writer};
use crate::expr::{Binding, NixExpr};
use crate::module::Assignment;

/// Rewrite `body` in place, replacing each maximal repeated compound value with a
/// reference to a generated binding, and return the bindings in first-use order.
pub fn hoist(body: &mut [Assignment]) -> Vec<Binding> {
    // Phase 1: count every eligible node, full descent, by emitted text.
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for a in body.iter() {
        count(&a.value, &mut counts);
    }
    let candidates: BTreeSet<String> = counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|(k, _)| k)
        .collect();
    if candidates.is_empty() {
        return Vec::new();
    }

    // Phase 2: maximal top-down replacement; names assigned on first encounter.
    let mut h = Hoister {
        candidates,
        names: BTreeMap::new(),
        bindings: Vec::new(),
        counter: 0,
    };
    for a in body.iter_mut() {
        let value = std::mem::replace(&mut a.value, NixExpr::Null);
        a.value = h.replace(value);
    }
    h.bindings
}

/// Emitted-text key for a subexpression: equality is defined by what lands in the file.
fn emit_key(expr: &NixExpr) -> String {
    let mut w = Writer::new();
    expr.emit(&mut w);
    w.into_string()
}

/// Compound values worth binding: non-empty attrsets and lists, and indented strings.
/// Scalars, refs, selects, applies, lambdas, lets, paths, and raw are never hoisted.
fn is_eligible(expr: &NixExpr) -> bool {
    match expr {
        NixExpr::AttrSet(m) => !m.is_empty(),
        NixExpr::List(items) => !items.is_empty(),
        NixExpr::IndentStr(_) => true,
        _ => false,
    }
}

/// Count eligible nodes by emitted text, descending into every child.
fn count(expr: &NixExpr, counts: &mut BTreeMap<String, usize>) {
    if is_eligible(expr) {
        *counts.entry(emit_key(expr)).or_insert(0) += 1;
    }
    for child in children(expr) {
        count(child, counts);
    }
}

/// The direct child expressions of a node, in deterministic order.
fn children(expr: &NixExpr) -> Vec<&NixExpr> {
    match expr {
        NixExpr::List(items) => items.iter().collect(),
        NixExpr::AttrSet(m) => m.values().collect(),
        NixExpr::Select(base, _) => vec![base],
        NixExpr::Apply(f, args) => std::iter::once(f.as_ref()).chain(args.iter()).collect(),
        NixExpr::Lambda { body, .. } => vec![body],
        NixExpr::Let { bindings, body } => bindings
            .iter()
            .map(|b| &b.value)
            .chain(std::iter::once(body.as_ref()))
            .collect(),
        _ => Vec::new(),
    }
}

struct Hoister {
    candidates: BTreeSet<String>,
    names: BTreeMap<String, String>,
    bindings: Vec<Binding>,
    counter: usize,
}

impl Hoister {
    /// Replace `expr` with a binding reference if it is a maximal repeated compound;
    /// otherwise recurse into its children. Stops descending at a hoisted node so the
    /// binding's interior stays literal and no binding references another.
    fn replace(&mut self, expr: NixExpr) -> NixExpr {
        if is_eligible(&expr) {
            let key = emit_key(&expr);
            if self.candidates.contains(&key) {
                let name = match self.names.get(&key) {
                    Some(n) => n.clone(),
                    None => {
                        let n = format!("_knixl{}", self.counter);
                        self.counter += 1;
                        self.names.insert(key, n.clone());
                        self.bindings.push(Binding {
                            name: n.clone(),
                            value: expr,
                        });
                        n
                    }
                };
                return NixExpr::Ref(name);
            }
        }
        self.recurse(expr)
    }

    fn recurse(&mut self, expr: NixExpr) -> NixExpr {
        match expr {
            NixExpr::List(items) => {
                NixExpr::List(items.into_iter().map(|e| self.replace(e)).collect())
            }
            NixExpr::AttrSet(m) => {
                NixExpr::AttrSet(m.into_iter().map(|(k, v)| (k, self.replace(v))).collect())
            }
            NixExpr::Select(base, path) => NixExpr::Select(Box::new(self.replace(*base)), path),
            NixExpr::Apply(f, args) => NixExpr::Apply(
                Box::new(self.replace(*f)),
                args.into_iter().map(|e| self.replace(e)).collect(),
            ),
            NixExpr::Lambda { formals, body } => NixExpr::Lambda {
                formals,
                body: Box::new(self.replace(*body)),
            },
            NixExpr::Let { bindings, body } => NixExpr::Let {
                bindings: bindings
                    .into_iter()
                    .map(|b| Binding {
                        name: b.name,
                        value: self.replace(b.value),
                    })
                    .collect(),
                body: Box::new(self.replace(*body)),
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::{AttrKey, AttrPath};
    use std::collections::BTreeMap;

    fn path(seg: &str) -> AttrPath {
        AttrPath(vec![AttrKey::Ident(seg.into())])
    }

    fn assign(seg: &str, value: NixExpr) -> Assignment {
        Assignment {
            path: path(seg),
            value,
            priority: None,
            condition: None,
            doc: None,
        }
    }

    fn attrset(pairs: &[(&str, NixExpr)]) -> NixExpr {
        let mut m = BTreeMap::new();
        for (k, v) in pairs {
            m.insert(AttrKey::Ident((*k).into()), v.clone());
        }
        NixExpr::AttrSet(m)
    }

    #[test]
    fn a_repeated_attrset_is_bound_once_and_referenced() {
        let shared = || attrset(&[("x", NixExpr::Int(1)), ("y", NixExpr::Int(2))]);
        let mut body = vec![assign("foo", shared()), assign("bar", shared())];

        let bindings = hoist(&mut body);

        assert_eq!(bindings.len(), 1, "one binding for the repeated attrset");
        assert_eq!(bindings[0].name, "_knixl0");
        // Both sites now reference the binding.
        assert!(matches!(&body[0].value, NixExpr::Ref(n) if n == "_knixl0"));
        assert!(matches!(&body[1].value, NixExpr::Ref(n) if n == "_knixl0"));
        // The binding holds the original attrset.
        assert!(matches!(&bindings[0].value, NixExpr::AttrSet(m) if m.len() == 2));
    }

    #[test]
    fn a_value_used_once_is_left_alone() {
        let mut body = vec![
            assign("foo", attrset(&[("x", NixExpr::Int(1))])),
            assign("bar", attrset(&[("y", NixExpr::Int(2))])),
        ];
        let bindings = hoist(&mut body);
        assert!(bindings.is_empty(), "distinct values are not hoisted");
        assert!(
            matches!(&body[0].value, NixExpr::AttrSet(_)),
            "value untouched"
        );
    }

    #[test]
    fn scalars_and_refs_are_never_hoisted() {
        let mut body = vec![
            assign("a", NixExpr::Int(1)),
            assign("b", NixExpr::Int(1)),
            assign("c", NixExpr::Ref("config".into())),
            assign("d", NixExpr::Ref("config".into())),
            assign("e", NixExpr::List(vec![])), // empty list: not eligible
            assign("f", NixExpr::List(vec![])),
        ];
        assert!(
            hoist(&mut body).is_empty(),
            "repeated scalars/refs/empties are not hoisted"
        );
    }

    #[test]
    fn hoisting_is_maximal_and_leaves_no_dangling_bindings() {
        // outer = { a = inner; }, inner = { x = 1; }. Both appear twice, but inner only
        // ever inside outer, so only outer is bound and inner stays literal.
        let inner = || attrset(&[("x", NixExpr::Int(1))]);
        let outer = || attrset(&[("a", inner())]);
        let mut body = vec![assign("foo", outer()), assign("bar", outer())];

        let bindings = hoist(&mut body);

        assert_eq!(bindings.len(), 1, "only the maximal (outer) value is bound");
        assert_eq!(bindings[0].name, "_knixl0");
        // The binding's interior is literal, not a reference.
        match &bindings[0].value {
            NixExpr::AttrSet(m) => {
                assert!(matches!(
                    m.get(&AttrKey::Ident("a".into())),
                    Some(NixExpr::AttrSet(_))
                ));
            }
            other => panic!("expected attrset binding, got {other:?}"),
        }
    }

    #[test]
    fn names_follow_first_use_order() {
        let a = || attrset(&[("a", NixExpr::Int(1))]);
        let b = || attrset(&[("b", NixExpr::Int(2))]);
        // b appears first in body order, so it takes _knixl0.
        let mut body = vec![
            assign("p", b()),
            assign("q", a()),
            assign("r", b()),
            assign("s", a()),
        ];

        let bindings = hoist(&mut body);

        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].name, "_knixl0");
        assert_eq!(bindings[1].name, "_knixl1");
        // _knixl0 is b (first encountered), referenced at p and r.
        assert!(matches!(&body[0].value, NixExpr::Ref(n) if n == "_knixl0"));
        assert!(matches!(&body[1].value, NixExpr::Ref(n) if n == "_knixl1"));
        assert!(matches!(&body[2].value, NixExpr::Ref(n) if n == "_knixl0"));
    }

    #[test]
    fn hoisting_is_deterministic_across_runs() {
        let shared = || attrset(&[("x", NixExpr::Int(1))]);
        let build = || vec![assign("foo", shared()), assign("bar", shared())];

        let mut b1 = build();
        let mut b2 = build();
        let r1 = hoist(&mut b1);
        let r2 = hoist(&mut b2);

        let names = |bs: &[Binding]| bs.iter().map(|b| b.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&r1), names(&r2), "binding names stable across runs");
    }
}
