use std::collections::BTreeMap;
use std::path::PathBuf; // camino::Utf8PathBuf is nicer here; std for the sketch.

/// A generated Nix value expression. Only what module bodies need, nothing more.
#[derive(Debug, Clone)]
pub enum NixExpr {
    Bool(bool),
    Int(i128),
    Float(f64),           // non-finite rejected at lower() time: Nix has no inf/nan
    Str(String),          // emitter owns escaping
    IndentStr(String),    // '' ... '' block, for multi-line config blobs
    Path(PathBuf),        // relative (./x) vs absolute is load-bearing in Nix
    Null,
    Ref(String),                          // bare var: config, lib, pkgs
    Select(Box<NixExpr>, Vec<String>),    // pkgs.hello -> Select(Ref("pkgs"), ["hello"])
    List(Vec<NixExpr>),
    AttrSet(BTreeMap<AttrKey, NixExpr>),  // BTreeMap => key order deterministic by construction
    Apply(Box<NixExpr>, Vec<NixExpr>),    // f a b
    Lambda { formals: Formals, body: Box<NixExpr> },
    Let { bindings: Vec<Binding>, body: Box<NixExpr> },
    Raw(RawNix),                          // escape hatch: validated, then passed through verbatim
}

/// Attribute key. The classifier decides bare vs quoted; the IR records intent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AttrKey {
    Ident(String),   // services
    Quoted(String),  // "example.com"
}

#[derive(Debug, Clone)]
pub struct AttrPath(pub Vec<AttrKey>);

impl AttrPath {
    /// Collapse dynamic quoted keys to "<name>" so the oracle can match option paths.
    /// services.nginx.virtualHosts."example.com".forceSSL
    ///   -> "services.nginx.virtualHosts.<name>.forceSSL"
    pub fn to_option_key(&self) -> String {
        todo!("Ident -> literal, Quoted -> <name>, join with '.'")
    }
    pub fn push(mut self, k: AttrKey) -> Self { self.0.push(k); self }
}

#[derive(Debug, Clone)]
pub enum Priority { Force, Default, Override(i64) }

#[derive(Debug, Clone)]
pub struct Formals { pub args: Vec<String>, pub ellipsis: bool }

#[derive(Debug, Clone)]
pub struct Binding { pub name: String, pub value: NixExpr }

#[derive(Debug, Clone)]
pub struct RawNix {
    pub src: String,
    pub span: Option<miette::SourceSpan>, // KDL origin, so raw-nix errors point at the KDL
}
