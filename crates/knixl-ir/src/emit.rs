//! The emitter. Deliberately not pretty: it produces structurally correct, stable Nix,
//! and a pinned nixfmt (knixl-nix) owns final layout. Only post-format text is hashed.
//! SPEC-GRADE SKETCH: escaping/float/attr-path helpers are declared, not written.

use crate::expr::{AttrKey, AttrPath, Formals, NixExpr, Priority, RawNix};
use crate::module::{Assignment, NixModule, Provenance};
use std::fmt::Write;

pub struct Writer {
    pub buf: String,
    indent: usize,
    at_line_start: bool,
}

impl Writer {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            indent: 0,
            at_line_start: true,
        }
    }
    pub fn into_string(self) -> String {
        self.buf
    }
    fn push(&mut self, s: &str) {
        if self.at_line_start {
            for _ in 0..self.indent {
                self.buf.push_str("  ");
            }
            self.at_line_start = false;
        }
        self.buf.push_str(s);
    }
    fn nl(&mut self) {
        self.buf.push('\n');
        self.at_line_start = true;
    }
    fn open(&mut self) {
        self.indent += 1;
    }
    fn close(&mut self) {
        self.indent = self.indent.saturating_sub(1);
    }
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

pub trait Emit {
    fn emit(&self, w: &mut Writer);
}

impl Emit for NixExpr {
    fn emit(&self, w: &mut Writer) {
        match self {
            NixExpr::Bool(b) => w.push(if *b { "true" } else { "false" }),
            NixExpr::Int(n) => {
                let _ = write!(w.buf, "{n}");
            }
            NixExpr::Float(f) => w.push(&fmt_nix_float(*f)), // canonical, always a '.'
            NixExpr::Null => w.push("null"),
            NixExpr::Str(s) => {
                w.push("\"");
                w.push(&escape_nix_str(s));
                w.push("\"");
            }
            NixExpr::IndentStr(s) => emit_indent_str(w, s), // '' ... '' with ''${ escaping
            NixExpr::Ref(id) => w.push(id),
            NixExpr::Path(p) => w.push(&p.display().to_string()),
            NixExpr::Select(base, path) => {
                // `(f x).y` and `(let .. in ..).y` need the base parenthesised; `pkgs.x`
                // and `{ .. }.x` stand alone.
                emit_atom(w, base);
                for seg in path {
                    w.push(".");
                    w.push(seg);
                }
            }
            NixExpr::List(items) => {
                w.push("[");
                w.nl();
                w.open();
                // List elements are whitespace-separated atoms, so a bare application or
                // binding form must be parenthesised or it splits into several elements.
                for it in items {
                    emit_atom(w, it);
                    w.nl();
                }
                w.close();
                w.push("]");
            }
            NixExpr::AttrSet(map) => {
                w.push("{");
                w.nl();
                w.open();
                for (k, v) in map {
                    // BTreeMap: sorted, deterministic
                    emit_key(w, k);
                    w.push(" = ");
                    v.emit(w);
                    w.push(";");
                    w.nl();
                }
                w.close();
                w.push("}");
            }
            NixExpr::Apply(f, args) => {
                // A lambda or let in function position needs wrapping (`(x: b) y`); a bare
                // ref/select/apply function is fine, so emit_atom leaves those alone.
                emit_atom(w, f);
                for a in args {
                    w.push(" (");
                    a.emit(w);
                    w.push(")");
                }
            }
            NixExpr::Lambda { formals, body } => {
                emit_formals(w, formals);
                w.push(": ");
                body.emit(w);
            }
            NixExpr::Let { bindings, body } => {
                w.push("let");
                w.nl();
                w.open();
                for b in bindings {
                    w.push(&b.name);
                    w.push(" = ");
                    b.value.emit(w);
                    w.push(";");
                    w.nl();
                }
                w.close();
                w.push("in ");
                body.emit(w);
            }
            NixExpr::Raw(raw) => emit_raw(w, raw), // already validated; verbatim
        }
    }
}

impl Emit for Assignment {
    fn emit(&self, w: &mut Writer) {
        if let Some(doc) = &self.doc {
            w.push("# ");
            w.push(doc);
            w.nl();
        }
        emit_attr_path(w, &self.path);
        w.push(" = ");
        // compose in one fixed order: mkIf cond (mkForce value)
        let mut close = 0;
        if let Some(cond) = &self.condition {
            w.push("lib.mkIf (");
            cond.emit(w);
            w.push(") ");
        }
        match &self.priority {
            Some(Priority::Force) => {
                w.push("(lib.mkForce ");
                close += 1;
            }
            Some(Priority::Default) => {
                w.push("(lib.mkDefault ");
                close += 1;
            }
            Some(Priority::Override(n)) => {
                let _ = write!(w.buf, "(lib.mkOverride {n} ");
                close += 1;
            }
            None => {}
        }
        self.value.emit(w);
        for _ in 0..close {
            w.push(")");
        }
        w.push(";");
        w.nl();
    }
}

impl Emit for NixModule {
    fn emit(&self, w: &mut Writer) {
        emit_header_comment(w, &self.provenance); // "Generated by knixl ... do NOT edit ..."
        emit_formals(w, &self.header);
        w.push(":");
        w.nl();
        // Hoisted bindings (let-hoisting pass) wrap the body: `let ... in { ... }`.
        if !self.lets.is_empty() {
            w.push("let");
            w.nl();
            w.open();
            for b in &self.lets {
                w.push(&b.name);
                w.push(" = ");
                b.value.emit(w);
                w.push(";");
                w.nl();
            }
            w.close();
            w.push("in "); // next push starts the body attrset: "in {"
        }
        w.push("{");
        w.nl();
        w.open();
        if !self.imports.is_empty() {
            w.push("imports = [");
            w.nl();
            w.open();
            for i in &self.imports {
                emit_atom(w, i);
                w.nl();
            }
            w.close();
            w.push("];");
            w.nl();
            w.nl();
        }
        for a in &self.body {
            a.emit(w);
        }
        for r in &self.raw {
            w.push("# raw-nix passthrough");
            w.nl();
            emit_raw(w, r);
            w.nl();
        }
        w.close();
        w.push("}");
        w.nl();
    }
}

// ---- helpers: declared, NOT written. These are the fiddly-but-boring bits. ----

fn fmt_nix_float(f: f64) -> String {
    // The IR contract rejects non-finite at lower() time (Nix has no inf/nan); assert
    // here so a bug upstream fails loudly rather than emitting invalid Nix.
    assert!(f.is_finite(), "non-finite float reached the emitter: {f}");
    // Rust's Display gives the shortest round-tripping decimal, but drops the point on
    // whole numbers ("1"), which Nix reads as an int. Force a decimal point.
    let s = format!("{f}");
    if s.contains(['.', 'e', 'E']) {
        s
    } else {
        format!("{s}.0")
    }
}

fn escape_nix_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Only ${ starts an interpolation; a lone $ is a literal dollar.
            '$' if chars.peek() == Some(&'{') => {
                out.push_str("\\${");
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// Escaping inside a `'' ... ''` indented string: `${` becomes `''${` and `''` becomes
/// `'''`. Everything else, including interior quotes, is literal.
fn escape_indent_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '$' if chars.peek() == Some(&'{') => {
                out.push_str("''${");
                chars.next();
            }
            '\'' if chars.peek() == Some(&'\'') => {
                out.push_str("'''");
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

fn emit_indent_str(w: &mut Writer, s: &str) {
    w.push("''");
    w.nl();
    w.open();
    for line in s.lines() {
        w.push(&escape_indent_line(line));
        w.nl();
    }
    w.close();
    w.push("''");
}

fn emit_attr_path(w: &mut Writer, p: &AttrPath) {
    for (i, key) in p.0.iter().enumerate() {
        if i > 0 {
            w.push(".");
        }
        emit_key(w, key);
    }
}

fn emit_key(w: &mut Writer, k: &AttrKey) {
    match k {
        AttrKey::Ident(s) if is_bare_ident(s) => w.push(s),
        // An Ident that is not a valid bare name, or an explicitly quoted key, is quoted.
        AttrKey::Ident(s) | AttrKey::Quoted(s) => {
            w.push("\"");
            w.push(&escape_nix_str(s));
            w.push("\"");
        }
    }
}

/// A Nix bare attribute name: `[A-Za-z_][A-Za-z0-9_'-]*`. Anything else must be quoted.
fn is_bare_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '\''))
}

fn emit_formals(w: &mut Writer, f: &Formals) {
    let mut items: Vec<String> = f.args.clone();
    if f.ellipsis {
        items.push("...".to_string());
    }
    if items.is_empty() {
        w.push("{ }");
    } else {
        w.push("{ ");
        w.push(&items.join(", "));
        w.push(" }");
    }
}

fn emit_raw(w: &mut Writer, r: &RawNix) {
    // Already validated as parseable Nix; passed through verbatim, honouring current indent.
    for (i, line) in r.src.lines().enumerate() {
        if i > 0 {
            w.nl();
        }
        w.push(line);
    }
}

/// Emit `e` where a single atom is required (a list element, a select base, or a function
/// position). Function application and binding forms (`f x`, `let .. in ..`, lambdas) are not
/// atoms, so they are parenthesised; everything else (refs, selects, literals, attrsets,
/// lists) already stands alone. Without this, `[ import (fetchGit ..) ({..}).x ]` splits into
/// separate list elements and `(import ..).x` loses its parens. `Raw` is opaque text and is
/// emitted verbatim: if a caller ever puts a raw application in atom position it must include
/// its own parens.
fn emit_atom(w: &mut Writer, e: &NixExpr) {
    if matches!(
        e,
        NixExpr::Apply(..) | NixExpr::Lambda { .. } | NixExpr::Let { .. }
    ) {
        w.push("(");
        e.emit(w);
        w.push(")");
    } else {
        e.emit(w);
    }
}

fn emit_header_comment(w: &mut Writer, p: &Provenance) {
    let sources = p
        .sources
        .iter()
        .map(|s| s.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    w.push(&format!(
        "# Generated by knixl {} from {sources}",
        p.tool_version
    ));
    w.nl();
    w.push("# Do NOT edit. Regenerate from the KDL source.");
    w.nl();
    w.push("# Overrides: add a sibling module and use lib.mkForce / lib.mkAfter.");
    w.nl();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::{AttrKey, AttrPath, Formals, NixExpr, RawNix};
    use crate::module::{ModuleRef, Provenance};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn capture(f: impl FnOnce(&mut Writer)) -> String {
        let mut w = Writer::new();
        f(&mut w);
        w.into_string()
    }

    // ---- fmt_nix_float ----

    #[test]
    fn float_whole_number_gets_a_decimal_point() {
        assert_eq!(fmt_nix_float(1.0), "1.0");
        assert_eq!(fmt_nix_float(-2.0), "-2.0");
        assert_eq!(fmt_nix_float(0.0), "0.0");
        assert_eq!(fmt_nix_float(100.0), "100.0");
    }

    #[test]
    fn float_fractional_is_shortest_round_trip() {
        assert_eq!(fmt_nix_float(1.5), "1.5");
        assert_eq!(fmt_nix_float(0.1), "0.1");
    }

    #[test]
    #[should_panic(expected = "non-finite")]
    fn float_rejects_infinity() {
        let _ = fmt_nix_float(f64::INFINITY);
    }

    #[test]
    #[should_panic(expected = "non-finite")]
    fn float_rejects_nan() {
        let _ = fmt_nix_float(f64::NAN);
    }

    // ---- escape_nix_str ----

    #[test]
    fn escape_plain_string_unchanged() {
        assert_eq!(
            escape_nix_str("http://127.0.0.1:3000"),
            "http://127.0.0.1:3000"
        );
    }

    #[test]
    fn escape_quotes_and_backslashes() {
        assert_eq!(escape_nix_str(r#"he said "hi""#), r#"he said \"hi\""#);
        assert_eq!(escape_nix_str(r"a\b"), r"a\\b");
    }

    #[test]
    fn escape_interpolation_open() {
        assert_eq!(escape_nix_str("${x}"), r"\${x}");
        // a lone dollar not starting an interpolation is left alone
        assert_eq!(escape_nix_str("$5.00"), "$5.00");
    }

    #[test]
    fn escape_control_chars() {
        assert_eq!(escape_nix_str("a\nb\tc\rd"), r"a\nb\tc\rd");
    }

    // ---- emit_indent_str ----

    #[test]
    fn indent_str_is_a_double_quote_block() {
        assert_eq!(capture(|w| emit_indent_str(w, "a\nb")), "''\n  a\n  b\n''");
    }

    #[test]
    fn indent_str_escapes_interpolation_and_double_quotes() {
        // ${ becomes ''${ and '' becomes '''
        assert_eq!(
            capture(|w| emit_indent_str(w, "x=${y}")),
            "''\n  x=''${y}\n''"
        );
        assert_eq!(capture(|w| emit_indent_str(w, "it''s")), "''\n  it'''s\n''");
    }

    // ---- emit_attr_path / emit_key ----

    #[test]
    fn attr_path_bare_and_quoted_segments() {
        let p = AttrPath(vec![
            AttrKey::Ident("services".into()),
            AttrKey::Ident("nginx".into()),
            AttrKey::Quoted("example.com".into()),
        ]);
        assert_eq!(
            capture(|w| emit_attr_path(w, &p)),
            r#"services.nginx."example.com""#
        );
    }

    #[test]
    fn key_dashed_identifier_stays_bare() {
        assert_eq!(
            capture(|w| emit_key(w, &AttrKey::Ident("recommended-tls".into()))),
            "recommended-tls"
        );
    }

    #[test]
    fn key_ident_needing_quotes_is_quoted() {
        // a leading digit is not a valid bare identifier
        assert_eq!(
            capture(|w| emit_key(w, &AttrKey::Ident("443".into()))),
            r#""443""#
        );
        assert_eq!(
            capture(|w| emit_key(w, &AttrKey::Quoted("/".into()))),
            r#""/""#
        );
    }

    // ---- emit_formals ----

    #[test]
    fn formals_with_args_and_ellipsis() {
        let f = Formals {
            args: vec!["config".into(), "lib".into(), "pkgs".into()],
            ellipsis: true,
        };
        assert_eq!(
            capture(|w| emit_formals(w, &f)),
            "{ config, lib, pkgs, ... }"
        );
    }

    #[test]
    fn formals_ellipsis_only() {
        let f = Formals {
            args: vec![],
            ellipsis: true,
        };
        assert_eq!(capture(|w| emit_formals(w, &f)), "{ ... }");
    }

    #[test]
    fn formals_args_no_ellipsis() {
        let f = Formals {
            args: vec!["x".into()],
            ellipsis: false,
        };
        assert_eq!(capture(|w| emit_formals(w, &f)), "{ x }");
    }

    // ---- emit_raw ----

    #[test]
    fn raw_is_verbatim() {
        let r = RawNix {
            src: "foo = 1;".into(),
            span: None,
        };
        assert_eq!(capture(|w| emit_raw(w, &r)), "foo = 1;");
    }

    #[test]
    fn raw_preserves_interior_newlines() {
        let r = RawNix {
            src: "a = 1;\nb = 2;".into(),
            span: None,
        };
        assert_eq!(capture(|w| emit_raw(w, &r)), "a = 1;\nb = 2;");
    }

    // ---- emit_header_comment ----

    #[test]
    fn header_comment_names_tool_and_sources() {
        let p = Provenance {
            tool_version: "0.3.1".parse().unwrap(),
            modules: vec![ModuleRef {
                name: "host".into(),
                version: "1.0.0".parse().unwrap(),
            }],
            sources: vec![PathBuf::from("hosts/web.kdl")],
        };
        let out = capture(|w| emit_header_comment(w, &p));
        assert_eq!(
            out,
            "# Generated by knixl 0.3.1 from hosts/web.kdl\n\
             # Do NOT edit. Regenerate from the KDL source.\n\
             # Overrides: add a sibling module and use lib.mkForce / lib.mkAfter.\n"
        );
    }

    // ---- Emit smoke tests (exercise the helpers through NixExpr) ----

    #[test]
    fn attrset_keys_emit_in_sorted_order() {
        let mut m = BTreeMap::new();
        m.insert(AttrKey::Ident("b".into()), NixExpr::Int(2));
        m.insert(AttrKey::Ident("a".into()), NixExpr::Int(1));
        assert_eq!(
            capture(|w| NixExpr::AttrSet(m).emit(w)),
            "{\n  a = 1;\n  b = 2;\n}"
        );
    }

    #[test]
    fn string_expr_is_quoted_and_escaped() {
        assert_eq!(
            capture(|w| NixExpr::Str(r#"a"b"#.into()).emit(w)),
            r#""a\"b""#
        );
    }

    // ---- determinism (the lock depends on it) ----

    #[test]
    fn attrset_emit_is_insertion_order_independent() {
        // AttrSet is a BTreeMap, so key order is fixed by construction: building the same
        // entries in any insertion order must emit identical bytes. This is the one
        // collection whose order must never leak into the output.
        let entries = [
            (AttrKey::Ident("z".into()), NixExpr::Int(1)),
            (AttrKey::Ident("a".into()), NixExpr::Int(2)),
            (AttrKey::Ident("m".into()), NixExpr::Int(3)),
        ];
        let mut forward = BTreeMap::new();
        for (k, v) in entries.iter().cloned() {
            forward.insert(k, v);
        }
        let mut reverse = BTreeMap::new();
        for (k, v) in entries.iter().rev().cloned() {
            reverse.insert(k, v);
        }
        assert_eq!(
            capture(|w| NixExpr::AttrSet(forward).emit(w)),
            capture(|w| NixExpr::AttrSet(reverse).emit(w)),
        );
    }

    #[test]
    fn attr_path_to_option_key_collapses_quoted_segments() {
        let p = AttrPath(vec![
            AttrKey::Ident("services".into()),
            AttrKey::Ident("nginx".into()),
            AttrKey::Ident("virtualHosts".into()),
            AttrKey::Quoted("example.com".into()),
            AttrKey::Ident("forceSSL".into()),
        ]);
        assert_eq!(
            p.to_option_key(),
            "services.nginx.virtualHosts.<name>.forceSSL"
        );
    }

    #[test]
    fn emitting_twice_is_byte_identical() {
        let expr = NixExpr::List(vec![
            NixExpr::Int(1),
            NixExpr::Str("x".into()),
            NixExpr::Bool(true),
        ]);
        assert_eq!(capture(|w| expr.emit(w)), capture(|w| expr.emit(w)));
    }

    // ---- atom parenthesisation (list elements and select bases) ----

    #[test]
    fn application_as_list_element_is_parenthesised() {
        // A bare `f (x)` in a list would split into two separate elements; it must be
        // wrapped so it stays a single atom.
        let expr = NixExpr::List(vec![NixExpr::Apply(
            Box::new(NixExpr::Ref("f".into())),
            vec![NixExpr::Ref("x".into())],
        )]);
        assert!(
            capture(|w| expr.emit(w)).contains("(f (x))"),
            "application list element must be parenthesised: {}",
            capture(|w| expr.emit(w))
        );
    }

    #[test]
    fn select_on_an_application_parenthesises_the_base() {
        // `(f x).y`, not `f x.y` (which parses as `f (x.y)`). This is the pinned-package
        // shape: `(import (fetchGit ..) { .. }).<name>`.
        let expr = NixExpr::Select(
            Box::new(NixExpr::Apply(
                Box::new(NixExpr::Ref("f".into())),
                vec![NixExpr::Ref("x".into())],
            )),
            vec!["y".into()],
        );
        assert_eq!(capture(|w| expr.emit(w)), "(f (x)).y");
    }

    #[test]
    fn select_on_a_ref_is_not_parenthesised() {
        let expr = NixExpr::Select(
            Box::new(NixExpr::Ref("pkgs".into())),
            vec!["ripgrep".into()],
        );
        assert_eq!(capture(|w| expr.emit(w)), "pkgs.ripgrep");
    }

    #[test]
    fn lambda_in_function_position_is_parenthesised() {
        // `({ x }: x) (5)`, not `{ x }: x (5)` (which parses as a lambda returning `x (5)`).
        let expr = NixExpr::Apply(
            Box::new(NixExpr::Lambda {
                formals: Formals {
                    args: vec!["x".into()],
                    ellipsis: false,
                },
                body: Box::new(NixExpr::Ref("x".into())),
            }),
            vec![NixExpr::Int(5)],
        );
        assert_eq!(capture(|w| expr.emit(w)), "({ x }: x) (5)");
    }

    // ---- module let-block ----

    fn module_with(lets: Vec<crate::expr::Binding>) -> NixModule {
        use crate::module::Assignment;
        NixModule {
            header: Formals {
                args: vec!["config".into(), "lib".into(), "pkgs".into()],
                ellipsis: true,
            },
            imports: vec![],
            lets,
            body: vec![Assignment {
                path: AttrPath(vec![AttrKey::Ident("foo".into())]),
                value: NixExpr::Ref("_knixl0".into()),
                priority: None,
                condition: None,
                doc: None,
            }],
            raw: vec![],
            provenance: Provenance {
                tool_version: "0.3.1".parse().unwrap(),
                modules: vec![],
                sources: vec![PathBuf::from("hosts/h.kdl")],
            },
        }
    }

    #[test]
    fn module_with_lets_emits_a_let_in_block() {
        use crate::expr::Binding;
        let mut binding_val = BTreeMap::new();
        binding_val.insert(AttrKey::Ident("x".into()), NixExpr::Int(1));
        let m = module_with(vec![Binding {
            name: "_knixl0".into(),
            value: NixExpr::AttrSet(binding_val),
        }]);

        let out = capture(|w| m.emit(w));
        assert!(out.contains("let"), "emits a let keyword:\n{out}");
        assert!(out.contains("_knixl0 = {"), "binding rendered:\n{out}");
        assert!(out.contains("in {"), "let/in block present:\n{out}");
        assert!(
            out.contains("foo = _knixl0;"),
            "body references the binding:\n{out}"
        );
    }

    #[test]
    fn module_without_lets_emits_a_plain_attrset() {
        let out = capture(|w| module_with(vec![]).emit(w));
        assert!(
            !out.contains("let"),
            "no let block when there are no bindings:\n{out}"
        );
    }
}
