//! `import`: a hand-written NixOS module the host file imports, for what no node expresses.
//! The path is relative to the host's KDL file; the pipeline rewrites it relative to the
//! generated file and merges it into the same `imports` list as the host's side-files.
use crate::{
    Field, ImportUnit, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, ValueTy,
};
use kdl::KdlNode;

pub struct Import {
    schema: NodeSchema,
}
impl Import {
    pub fn new() -> Self {
        Self { schema: schema() }
    }
}
impl Default for Import {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Import {
    fn id(&self) -> ModuleId {
        ModuleId {
            name: "import".into(),
            version: "1.0.0".parse().unwrap(),
        }
    }
    fn node_name(&self) -> &str {
        "import"
    }
    fn schema(&self) -> &NodeSchema {
        &self.schema
    }
    fn lower(&self, node: &KdlNode, ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let mut out = LowerOutput::new();
        let Some(path) = knixl_kdl::first_arg_str(node) else {
            return Err(LowerError::missing("import path"));
        };
        match check_path(&path) {
            Ok(()) => out.imports.push(ImportUnit {
                path,
                module: String::new(),
            }),
            Err(why) => ctx.reject(node.span(), format!("import \"{path}\": {why}")),
        }
        Ok(out)
    }
}

/// The path is spliced into the generated file as a bare Nix path literal, so anything
/// outside the path-literal alphabet would change the Nix, not just the path.
fn check_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("the path is empty");
    }
    if path.starts_with('/') {
        return Err("the path must be relative to the host's KDL file");
    }
    if !path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '/'))
    {
        return Err("the path may only contain letters, digits, `.`, `_`, `-`, `+` and `/`");
    }
    Ok(())
}

fn schema() -> NodeSchema {
    NodeSchema {
        summary: "Import a hand-written NixOS module into the host file.".into(),
        args: vec![Field {
            name: "path".into(),
            ty: ValueTy::Str,
            required: true,
            doc: "Path to a .nix file or a directory with a default.nix, relative to the host's \
                  KDL file. It must stay inside the project."
                .into(),
        }],
        props: vec![],
        children: vec![],
        open_children: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Registry, Scope};

    fn lower(src: &str) -> (LowerOutput, Vec<crate::Diagnostic>) {
        let node = src
            .parse::<kdl::KdlDocument>()
            .unwrap()
            .nodes()
            .first()
            .unwrap()
            .clone();
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags, vec![]);
        let out = Import::new().lower(&node, &mut ctx).unwrap();
        (out, diags)
    }

    #[test]
    fn import_records_the_path_as_written() {
        let (out, diags) = lower("import \"../modules/kvm-dst\"");
        assert!(diags.is_empty());
        assert_eq!(out.imports.len(), 1);
        assert_eq!(out.imports[0].path, "../modules/kvm-dst");
    }

    #[test]
    fn import_refuses_an_absolute_path() {
        let (out, diags) = lower("import \"/etc/nixos/x.nix\"");
        assert!(out.imports.is_empty());
        assert!(
            diags[0].message.contains("relative"),
            "{}",
            diags[0].message
        );
    }

    #[test]
    fn import_refuses_characters_that_would_change_the_nix() {
        let (out, diags) = lower("import \"./x.nix; y\"");
        assert!(out.imports.is_empty());
        assert_eq!(diags.len(), 1);
    }
}
