//! Best-effort parse of the option type DESCRIPTION string from options.json.
//! Unknown descriptions accept everything (punt), which is the safe direction.

use knixl_ir::NixExpr;

#[derive(Debug, Clone)]
pub enum NixType {
    Bool, Int, Float, Str, Path, Package,
    List(Box<NixType>),
    AttrsOf(Box<NixType>),
    NullOr(Box<NixType>),
    Enum(Vec<String>),
    OneOf(Vec<NixType>),
    Submodule,          // interior not checked
    Unknown(String),    // description we could not parse; accept() returns Ok
}

impl NixType {
    /// "boolean" -> Bool; "list of string" -> List(Str);
    /// "null or (attribute set of package)" -> NullOr(AttrsOf(Package));
    /// "one of \"a\", \"b\"" -> Enum(...); anything else -> Unknown(s).
    pub fn parse_description(s: &str) -> NixType {
        parse_type_desc(s).unwrap_or_else(|| NixType::Unknown(s.to_string()))
    }

    /// Ok(()) if the value is acceptable, Err(expected) otherwise.
    /// Unknown and Submodule always return Ok (punt).
    pub fn accepts(&self, _v: &NixExpr) -> Result<(), String> {
        todo!("structural accept check; Unknown/Submodule => Ok")
    }
}

fn parse_type_desc(_s: &str) -> Option<NixType> {
    todo!("small recursive-descent parser over the description grammar")
}
