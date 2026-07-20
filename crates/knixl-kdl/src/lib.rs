//! Input parsing over the `kdl` crate (v2 by default). Thin: parse to KdlDocument,
//! carry spans for diagnostics. Small helpers modules use to read args/props/children.
//! SPEC-GRADE SKETCH.

use kdl::{KdlDocument, KdlNode};

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum ParseError {
    #[error("failed to parse KDL")]
    Kdl(#[from] kdl::KdlError),
}

pub fn parse(src: &str) -> Result<KdlDocument, ParseError> {
    Ok(src.parse::<KdlDocument>()?)
}

// Small readers used across modules. Kept here so module code stays declarative.

pub fn child_arg_str(node: &KdlNode, child: &str) -> Option<String> {
    children_named(node, child)
        .next()
        .and_then(|c| c.entries().iter().find(|e| e.name().is_none()))
        .and_then(|e| e.value().as_string())
        .map(str::to_string)
}

pub fn child_prop_str(node: &KdlNode, child: &str, prop: &str) -> Option<String> {
    children_named(node, child)
        .next()
        .and_then(|c| c.get(prop))
        .and_then(|v| v.as_string())
        .map(str::to_string)
}

pub fn child_flag(node: &KdlNode, child: &str) -> Option<bool> {
    let child = children_named(node, child).next()?;
    // An explicit boolean argument wins; a bare present child means true.
    let explicit = child
        .entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_bool());
    Some(explicit.unwrap_or(true))
}

pub fn children_named<'a>(
    node: &'a KdlNode,
    name: &'a str,
) -> impl Iterator<Item = &'a KdlNode> + 'a {
    node.children()
        .into_iter()
        .flat_map(|doc| doc.nodes().iter())
        .filter(move |n| n.name().value() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_node(src: &str) -> KdlNode {
        parse(src)
            .expect("parse")
            .nodes()
            .first()
            .expect("a node")
            .clone()
    }

    #[test]
    fn child_arg_str_reads_first_positional() {
        let n = first_node("host \"web\" {\n    system \"x86_64-linux\"\n}");
        assert_eq!(
            child_arg_str(&n, "system"),
            Some("x86_64-linux".to_string())
        );
        assert_eq!(child_arg_str(&n, "absent"), None);
    }

    #[test]
    fn child_prop_str_reads_named_property() {
        let n = first_node("svc {\n    acme email=\"ops@example.com\"\n}");
        assert_eq!(
            child_prop_str(&n, "acme", "email"),
            Some("ops@example.com".to_string())
        );
        assert_eq!(child_prop_str(&n, "acme", "missing"), None);
        assert_eq!(child_prop_str(&n, "absent", "email"), None);
    }

    #[test]
    fn child_flag_reads_bool_or_treats_presence_as_true() {
        let n = first_node("svc {\n    hardened #true\n    disabled #false\n    bare\n}");
        assert_eq!(child_flag(&n, "hardened"), Some(true));
        assert_eq!(child_flag(&n, "disabled"), Some(false));
        assert_eq!(child_flag(&n, "bare"), Some(true));
        assert_eq!(child_flag(&n, "absent"), None);
    }

    #[test]
    fn children_named_preserves_source_order() {
        let n = first_node("pg {\n    database \"app\"\n    database \"metrics\"\n}");
        let names: Vec<String> = children_named(&n, "database")
            .filter_map(child_first_arg)
            .collect();
        assert_eq!(names, vec!["app".to_string(), "metrics".to_string()]);
    }

    fn child_first_arg(node: &KdlNode) -> Option<String> {
        node.entries()
            .iter()
            .find(|e| e.name().is_none())
            .and_then(|e| e.value().as_string())
            .map(str::to_string)
    }
}
