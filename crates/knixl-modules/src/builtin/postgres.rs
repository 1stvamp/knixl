//! `postgres`: the canonical BUILT-IN case. "Force the override only if the user's input
//! conflicts with the base preset" is conditional priority computation, which a declarative
//! template cannot express. That is the whole reason this is Rust, not KDL.
use kdl::KdlNode;
use knixl_ir::{NixExpr, Priority};
use knixl_kdl::{child_flag, children_named};
use crate::builtin::host::{assign, unit_default};
use crate::{LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema};

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

fn schema() -> NodeSchema { todo!("postgres schema: prop version:int; child listen-tcp flag; repeated child database:string") }
