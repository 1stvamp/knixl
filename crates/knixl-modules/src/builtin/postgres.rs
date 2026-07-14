//! `postgres`: the canonical BUILT-IN case. "Force the override only if the user's input
//! conflicts with the base preset" is conditional priority computation, which a declarative
//! template cannot express. That is the whole reason this is Rust, not KDL.
use kdl::KdlNode;
use knixl_ir::{NixExpr, Priority};
use knixl_kdl::{child_flag, children_named};
use crate::builtin::host::{assign, unit_default};
use crate::{Child, Field, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, ValueTy};

pub struct Postgres { schema: NodeSchema }
impl Postgres { pub fn new() -> Self { Self { schema: schema() } } }
impl Default for Postgres { fn default() -> Self { Self::new() } }

impl Module for Postgres {
    fn id(&self) -> ModuleId { ModuleId { name: "postgres".into(), version: "0.4.0".parse().unwrap() } }
    fn node_name(&self) -> &str { "postgres" }
    fn schema(&self) -> &NodeSchema { &self.schema }
    fn lower(&self, node: &KdlNode, _ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let major = node.get("version").and_then(|v| v.as_integer()).unwrap_or(16);
        let listen_tcp = child_flag(node, "listen-tcp").unwrap_or(false);
        let dbs: Vec<String> = children_named(node, "database")
            .filter_map(|c| c.entries().first().and_then(|e| e.value().as_string()).map(str::to_owned))
            .collect();

        let mut units = vec![
            unit_default(assign(&["services","postgresql","enable"], NixExpr::Bool(true))),
            unit_default(assign(&["services","postgresql","package"],
                NixExpr::Select(Box::new(NixExpr::Ref("pkgs".into())),
                                vec![format!("postgresql_{major}")]))),
            unit_default(assign(&["services","postgresql","ensureDatabases"],
                NixExpr::List(dbs.into_iter().map(NixExpr::Str).collect()))),
        ];

        // The base preset sets enableTCPIP = false. Setting it again is a value conflict
        // unless we force. Whether we force depends on an INPUT flag => not declarative.
        if listen_tcp {
            let mut a = assign(&["services","postgresql","enableTCPIP"], NixExpr::Bool(true));
            a.priority = Some(Priority::Force);
            a.doc = Some("listen-tcp #true overrides the base preset (enableTCPIP = false)".into());
            units.push(unit_default(a));
        }
        Ok(LowerOutput { units })
    }
}

fn schema() -> NodeSchema {
    NodeSchema {
        summary: "A PostgreSQL server with the base hardening preset.".into(),
        args: vec![],
        props: vec![Field {
            name: "version".into(),
            ty: ValueTy::Int,
            required: false,
            doc: "Major version, e.g. 16. Defaults to 16.".into(),
        }],
        children: vec![
            Child {
                name: "listen-tcp".into(),
                ty: ValueTy::Bool,
                required: false,
                repeated: false,
                delegate: false,
                doc: "Listen on TCP/IP, forcing past the base preset's enableTCPIP = false.".into(),
            },
            Child {
                name: "database".into(),
                ty: ValueTy::Str,
                required: false,
                repeated: true,
                delegate: false,
                doc: "A database to ensure exists. Repeatable.".into(),
            },
        ],
        open_children: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Registry, Scope};

    fn node(src: &str) -> KdlNode {
        src.parse::<kdl::KdlDocument>().unwrap().nodes().first().unwrap().clone()
    }

    #[test]
    fn postgres_forces_tcp_only_when_requested_and_lists_databases() {
        let pg = Postgres::new();
        let n = node("postgres version=16 {\n    listen-tcp #true\n    database \"app\"\n    database \"metrics\"\n}");
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "db".into() }, &reg, &mut diags);

        let out = pg.lower(&n, &mut ctx).unwrap();
        // enable, package, ensureDatabases, and the forced enableTCPIP
        assert_eq!(out.units.len(), 4);
        let forced = out.units.last().unwrap();
        assert!(matches!(forced.assignment.priority, Some(Priority::Force)));
    }

    #[test]
    fn postgres_without_listen_tcp_omits_the_forced_override() {
        let pg = Postgres::new();
        let n = node("postgres {\n    database \"app\"\n}");
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "db".into() }, &reg, &mut diags);

        let out = pg.lower(&n, &mut ctx).unwrap();
        assert_eq!(out.units.len(), 3);
        assert!(out.units.iter().all(|u| u.assignment.priority.is_none()));
    }
}
