//! Best-effort parse of the option type DESCRIPTION string from options.json.
//! Unknown descriptions accept everything (punt), which is the safe direction.

use knixl_ir::NixExpr;

#[derive(Debug, Clone)]
pub enum NixType {
    Bool,
    Int,
    Float,
    Str,
    Path,
    Package,
    List(Box<NixType>),
    AttrsOf(Box<NixType>),
    NullOr(Box<NixType>),
    Enum(Vec<String>),
    OneOf(Vec<NixType>),
    Submodule,       // interior not checked
    Unknown(String), // description we could not parse; accept() returns Ok
}

impl NixType {
    /// "boolean" -> Bool; "list of string" -> List(Str);
    /// "null or (attribute set of package)" -> NullOr(AttrsOf(Package));
    /// "one of \"a\", \"b\"" -> Enum(...); anything else -> Unknown(s).
    pub fn parse_description(s: &str) -> NixType {
        parse_type_desc(s).unwrap_or_else(|| NixType::Unknown(s.to_string()))
    }

    /// Ok(()) if the value is acceptable, Err(expected) otherwise. Best-effort: only a
    /// literal of a clearly-wrong kind is rejected; non-literal expressions (refs, selects,
    /// applies, raw) and Unknown/Submodule/Package always pass.
    pub fn accepts(&self, v: &NixExpr) -> Result<(), String> {
        use NixExpr::*;
        match self {
            NixType::Unknown(_) | NixType::Submodule | NixType::Package => Ok(()),
            NixType::Bool => scalar(v, matches!(v, Bool(_)), "boolean"),
            NixType::Int => scalar(v, matches!(v, Int(_)), "integer"),
            NixType::Float => scalar(v, matches!(v, Float(_) | Int(_)), "floating point number"),
            NixType::Str => scalar(v, matches!(v, Str(_) | IndentStr(_)), "string"),
            NixType::Path => scalar(v, matches!(v, Path(_) | Str(_)), "path"),
            NixType::Enum(variants) => match v {
                Str(s) if variants.contains(s) => Ok(()),
                Str(_) => Err(format!("one of {}", variants.join(", "))),
                other => punt(other, "enum"),
            },
            NixType::List(inner) => match v {
                List(items) => items.iter().try_for_each(|it| inner.accepts(it)),
                other => punt(other, "list"),
            },
            NixType::AttrsOf(inner) => match v {
                AttrSet(map) => map.values().try_for_each(|val| inner.accepts(val)),
                other => punt(other, "attribute set"),
            },
            NixType::NullOr(inner) => match v {
                Null => Ok(()),
                other => inner.accepts(other),
            },
            NixType::OneOf(types) => {
                if types.iter().any(|t| t.accepts(v).is_ok()) {
                    Ok(())
                } else {
                    Err("one of several types".to_string())
                }
            }
        }
    }
}

/// The contents of `s` when one pair of parentheses wraps the whole string. `(a) or (b)` is not
/// wrapped, so stripping its first and last character would corrupt it into `a) or (b`.
fn strip_enclosing_parens(s: &str) -> Option<&str> {
    let inner = s.strip_prefix('(')?;
    let mut depth = 1usize;
    for (i, c) in inner.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    // Only encloses the whole string if this is the final character.
                    return (i + 1 == inner.len()).then(|| &inner[..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split on `sep` at paren depth zero, so a separator inside a nested type is left alone.
fn split_top_level(s: &str, sep: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            // Byte comparison, not `s[i..].starts_with`: a description can hold multibyte
            // characters (`3×3 matrix of floating point numbers`) and slicing mid-character
            // panics. A match is at an ASCII separator, so the slices below stay on boundaries.
            _ if depth == 0 && s.as_bytes()[i..].starts_with(sep.as_bytes()) => {
                parts.push(s[start..i].to_string());
                i += sep.len();
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(s[start..].to_string());
    parts
}

/// A literal of the wrong kind is a real mismatch; a non-literal expression can't be
/// checked structurally, so it passes.
fn scalar(v: &NixExpr, ok: bool, expected: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        punt(v, expected)
    }
}

fn punt(v: &NixExpr, expected: &str) -> Result<(), String> {
    if is_literal(v) {
        Err(expected.to_string())
    } else {
        Ok(())
    }
}

fn is_literal(v: &NixExpr) -> bool {
    use NixExpr::*;
    matches!(
        v,
        Bool(_) | Int(_) | Float(_) | Str(_) | IndentStr(_) | Path(_) | Null | List(_) | AttrSet(_)
    )
}

/// Best-effort parse of the common option-type descriptions. Anything unrecognised falls
/// through to `Unknown` (which accepts everything), the safe direction.
fn parse_type_desc(s: &str) -> Option<NixType> {
    let s = s.trim();
    if let Some(inner) = strip_enclosing_parens(s) {
        return Some(NixType::parse_description(inner));
    }
    if let Some(rest) = s.strip_prefix("null or ") {
        return Some(NixType::NullOr(Box::new(NixType::parse_description(rest))));
    }
    // A top-level union, how nixosOptionsDoc renders an `either`: `(attribute set of boolean) or
    // (list of string)`. Without splitting it, the substring fallbacks below match one
    // alternative and type the whole option as that alternative alone, which is stricter than
    // the option is and rejects legitimate values.
    let alts = split_top_level(s, " or ");
    if alts.len() > 1 {
        return Some(NixType::OneOf(
            alts.iter().map(|a| NixType::parse_description(a)).collect(),
        ));
    }
    if let Some(rest) = s.strip_prefix("list of ") {
        return Some(NixType::List(Box::new(NixType::parse_description(rest))));
    }
    if let Some(rest) = s.strip_prefix("attribute set of ") {
        return Some(NixType::AttrsOf(Box::new(NixType::parse_description(rest))));
    }
    if s.starts_with("one of ") {
        // Variants are the double-quoted tokens: `one of "a", "b"`.
        let variants: Vec<String> = s
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect();
        if !variants.is_empty() {
            return Some(NixType::Enum(variants));
        }
    }
    match s {
        "boolean" => Some(NixType::Bool),
        "string" => Some(NixType::Str),
        "path" => Some(NixType::Path),
        "package" | "derivation" => Some(NixType::Package),
        "floating point number" => Some(NixType::Float),
        _ if s.contains("submodule") => Some(NixType::Submodule),
        _ if s.contains("integer") => Some(NixType::Int),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knixl_ir::AttrKey;
    use std::collections::BTreeMap;

    #[test]
    fn parses_common_descriptions() {
        assert!(matches!(
            NixType::parse_description("boolean"),
            NixType::Bool
        ));
        assert!(
            matches!(NixType::parse_description("list of string"), NixType::List(b) if matches!(*b, NixType::Str))
        );
        assert!(matches!(
            NixType::parse_description("null or (attribute set of package)"),
            NixType::NullOr(b) if matches!(*b, NixType::AttrsOf(_))
        ));
        assert!(
            matches!(NixType::parse_description("one of \"a\", \"b\""), NixType::Enum(v) if v == ["a", "b"])
        );
        assert!(matches!(
            NixType::parse_description("some weird type"),
            NixType::Unknown(_)
        ));
    }

    #[test]
    fn accepts_matching_literals_and_rejects_wrong_ones() {
        assert!(NixType::Bool.accepts(&NixExpr::Bool(true)).is_ok());
        assert!(NixType::Bool.accepts(&NixExpr::Str("x".into())).is_err());
        assert!(NixType::Str.accepts(&NixExpr::Str("x".into())).is_ok());
        assert!(NixType::List(Box::new(NixType::Str))
            .accepts(&NixExpr::List(vec![NixExpr::Str("a".into())]))
            .is_ok());
        assert!(NixType::List(Box::new(NixType::Str))
            .accepts(&NixExpr::List(vec![NixExpr::Int(1)]))
            .is_err());
    }

    /// The real `environment.sessionVariables` description. Before unions were split, the
    /// `contains("integer")` fallback typed the whole thing as Int and a string value was
    /// rejected.
    #[test]
    fn a_union_inside_an_attrset_accepts_every_alternative() {
        let ty = NixType::parse_description(
            "attribute set of (null or (list of (signed integer or string or path)) or signed integer or string or path)",
        );
        let attrs = |v: NixExpr| {
            let mut m = BTreeMap::new();
            m.insert(AttrKey::Ident("KDIR".into()), v);
            NixExpr::AttrSet(m)
        };
        assert!(ty
            .accepts(&attrs(NixExpr::Str("/var/lib/bench".into())))
            .is_ok());
        assert!(ty.accepts(&attrs(NixExpr::Int(3))).is_ok());
        assert!(ty.accepts(&attrs(NixExpr::Null)).is_ok());
        assert!(ty
            .accepts(&attrs(NixExpr::List(vec![NixExpr::Str("a".into())])))
            .is_ok());
    }

    /// The real `boot.kernelModules` description. Its outer parens do not wrap the whole
    /// string, so a naive strip corrupted it and typed the option as an attrset only.
    #[test]
    fn a_top_level_union_of_bracketed_types_accepts_either_side() {
        let ty = NixType::parse_description("(attribute set of boolean) or (list of string)");
        assert!(ty
            .accepts(&NixExpr::List(vec![NixExpr::Str("br_netfilter".into())]))
            .is_ok());
        let mut m = BTreeMap::new();
        m.insert(AttrKey::Ident("br_netfilter".into()), NixExpr::Bool(true));
        assert!(ty.accepts(&NixExpr::AttrSet(m)).is_ok());
        // Still a real check: neither alternative admits a bare integer.
        assert!(ty.accepts(&NixExpr::Int(1)).is_err());
    }

    #[test]
    fn enclosing_parens_are_only_stripped_when_they_wrap_everything() {
        assert_eq!(
            strip_enclosing_parens("(list of string)"),
            Some("list of string")
        );
        assert_eq!(strip_enclosing_parens("(a) or (b)"), None);
        assert_eq!(strip_enclosing_parens("list of string"), None);
    }

    #[test]
    fn top_level_split_ignores_separators_inside_parens() {
        assert_eq!(
            split_top_level("(a or b) or c", " or "),
            vec!["(a or b)".to_string(), "c".to_string()]
        );
        assert_eq!(split_top_level("a", " or "), vec!["a".to_string()]);
    }

    /// A real nixpkgs description with a multibyte character. Scanning it by byte offset and
    /// slicing mid-character panicked, which took down every command that loads the oracle.
    #[test]
    fn multibyte_descriptions_do_not_panic() {
        let desc = "3×3 matrix of floating point numbers";
        assert_eq!(split_top_level(desc, " or "), vec![desc.to_string()]);
        let _ = NixType::parse_description(desc);
        let _ = NixType::parse_description("null or 3×3 matrix of floating point numbers");
    }

    #[test]
    fn punts_on_non_literals_and_unknown() {
        // A ref/select can't be checked structurally, so it passes any type.
        assert!(NixType::Bool
            .accepts(&NixExpr::Ref("config".into()))
            .is_ok());
        assert!(NixType::Unknown("x".into())
            .accepts(&NixExpr::Int(1))
            .is_ok());
        assert!(NixType::Package
            .accepts(&NixExpr::Str("anything".into()))
            .is_ok());
    }

    #[test]
    fn attrs_of_checks_values() {
        let mut m = BTreeMap::new();
        m.insert(AttrKey::Ident("a".into()), NixExpr::Bool(true));
        assert!(NixType::AttrsOf(Box::new(NixType::Bool))
            .accepts(&NixExpr::AttrSet(m.clone()))
            .is_ok());
        m.insert(AttrKey::Ident("b".into()), NixExpr::Str("no".into()));
        assert!(NixType::AttrsOf(Box::new(NixType::Bool))
            .accepts(&NixExpr::AttrSet(m))
            .is_err());
    }
}
