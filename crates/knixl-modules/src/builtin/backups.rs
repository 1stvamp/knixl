//! `backups`: scheduled restic backups gated on a runtime `lib.mkIf` off `config.*`.
//! The `when` condition is a raw Nix expression, which is exactly why this is a built-in:
//! a declarative module cannot emit a runtime condition (see docs/03).
use std::collections::BTreeMap;
use kdl::KdlNode;
use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr, RawNix};
use knixl_kdl::child_arg_str;
use crate::{Bucket, Child, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, Unit, ValueTy};

pub struct Backups { schema: NodeSchema }
impl Backups { pub fn new() -> Self { Self { schema: schema() } } }
impl Default for Backups { fn default() -> Self { Self::new() } }

impl Module for Backups {
    fn id(&self) -> ModuleId { ModuleId { name: "backups".into(), version: "0.2.1".parse().unwrap() } }
    fn node_name(&self) -> &str { "backups" }
    fn schema(&self) -> &NodeSchema { &self.schema }
    fn lower(&self, node: &KdlNode, ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let host = ctx.scope().host.clone();
        let repo = child_arg_str(node, "repo").ok_or_else(|| LowerError::missing("backups.repo"))?;
        let schedule = child_arg_str(node, "schedule").unwrap_or_else(|| "daily".into());
        let when = child_arg_str(node, "when");

        let mut timer = BTreeMap::new();
        timer.insert(AttrKey::Ident("OnCalendar".into()), NixExpr::Str(schedule));

        let mut set = BTreeMap::new();
        set.insert(AttrKey::Ident("repository".into()), NixExpr::Str(repo));
        set.insert(AttrKey::Ident("initialize".into()), NixExpr::Bool(true));
        set.insert(AttrKey::Ident("timerConfig".into()), NixExpr::AttrSet(timer));
        set.insert(
            AttrKey::Ident("pruneOpts".into()),
            NixExpr::List(vec![
                NixExpr::Str("--keep-daily 7".into()),
                NixExpr::Str("--keep-weekly 4".into()),
            ]),
        );

        let assignment = Assignment {
            path: AttrPath(vec![
                AttrKey::Ident("services".into()),
                AttrKey::Ident("restic".into()),
                AttrKey::Ident("backups".into()),
                AttrKey::Ident(host),
            ]),
            value: NixExpr::AttrSet(set),
            priority: None,
            // A runtime condition off config.*; emitted as `lib.mkIf <cond> <value>`.
            condition: when.map(|src| NixExpr::Raw(RawNix { src, span: None })),
            doc: None,
        };

        Ok(LowerOutput::units(vec![Unit { bucket: Bucket::Named("backup".into()), assignment }]))
    }
}

fn schema() -> NodeSchema {
    NodeSchema {
        summary: "Scheduled restic backups, gated on a runtime condition.".into(),
        args: vec![],
        props: vec![],
        children: vec![
            leaf("repo", "The restic repository URL.", true),
            leaf("schedule", "A systemd OnCalendar expression (default: daily).", false),
            leaf("when", "A raw Nix condition; the backup is wrapped in lib.mkIf.", false),
        ],
        open_children: false,
    }
}

fn leaf(name: &str, doc: &str, required: bool) -> Child {
    Child {
        name: name.into(),
        ty: ValueTy::Str,
        required,
        repeated: false,
        delegate: false,
        doc: doc.into(),
        args: vec![],
        props: vec![],
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
    fn backups_emits_a_named_side_file_gated_by_when() {
        let b = Backups::new();
        let n = node("backups {\n    repo \"s3:https://s3.example.com/backups/db\"\n    schedule \"daily\"\n    when \"config.services.postgresql.enable\"\n}");
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "db".into() }, &reg, &mut diags);

        let out = b.lower(&n, &mut ctx).unwrap();
        assert_eq!(out.units.len(), 1);
        let unit = &out.units[0];
        assert!(matches!(&unit.bucket, Bucket::Named(n) if n == "backup"));

        let a = &unit.assignment;
        let keys: Vec<&str> = a.path.0.iter().map(|k| match k {
            AttrKey::Ident(s) | AttrKey::Quoted(s) => s.as_str(),
        }).collect();
        assert_eq!(keys, vec!["services", "restic", "backups", "db"]);
        assert!(matches!(&a.condition, Some(NixExpr::Raw(r)) if r.src == "config.services.postgresql.enable"));
        assert!(matches!(&a.value, NixExpr::AttrSet(_)));
    }

    #[test]
    fn backups_condition_is_absent_without_when() {
        let b = Backups::new();
        let n = node("backups {\n    repo \"r\"\n}");
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "db".into() }, &reg, &mut diags);
        let out = b.lower(&n, &mut ctx).unwrap();
        assert!(out.units[0].assignment.condition.is_none());
    }
}
