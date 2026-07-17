//! knixl.lock.kdl: KDL for grep/diff friendliness. Records everything on the
//! reproducibility boundary: tool, formatter, oracle rev, and per-file hashes.
use std::collections::BTreeMap;
use std::path::PathBuf;
use kdl::{KdlDocument, KdlNode};
use semver::Version;

pub type Hash = String; // "blake3:...."

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lock {
    pub version: u32,
    pub tool: Version,
    pub formatter: FormatterPin,
    pub oracle: OraclePin,
    pub inputs: BTreeMap<PathBuf, Hash>,
    pub modules: BTreeMap<String, Version>,
    pub outputs: Vec<OutputEntry>,
    pub pins: BTreeMap<String, Vec<Pin>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatterPin { pub name: String, pub version: String }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OraclePin { pub nixpkgs_rev: String, pub options_hash: Hash }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputEntry {
    pub path: PathBuf,
    pub hash: Hash,
    pub from: PathBuf,
    pub modules: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub package: String,
    pub version: String,
    pub nixpkgs_rev: String,
    pub strategy: PinStrategy,
}

/// How a pinned package is emitted from its `nixpkgs-rev`. `CommitMix` (the default) imports
/// the whole package from the historical commit; `Override` builds the baseline package with
/// the historical `version` and `src` via `overrideAttrs`. Both record the same rev; only the
/// generated Nix differs. `Override` renders a `strategy="override"` attr; `CommitMix` renders
/// no attr at all, so existing locks (predating this field) round-trip byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinStrategy {
    CommitMix,
    Override,
}

impl Lock {
    pub fn parse(src: &str) -> Result<Lock, LockError> {
        let doc = src
            .parse::<KdlDocument>()
            .map_err(|e| LockError::Malformed(e.to_string()))?;
        let lock = doc
            .nodes()
            .iter()
            .find(|n| n.name().value() == "lock")
            .ok_or_else(|| LockError::Malformed("missing `lock` node".into()))?;
        let version = lock
            .get("version")
            .and_then(|v| v.as_integer())
            .ok_or_else(|| LockError::Malformed("`lock` missing integer prop `version`".into()))?
            as u32;
        let body = lock
            .children()
            .ok_or_else(|| LockError::Malformed("`lock` has no body".into()))?;

        let mut tool = None;
        let mut formatter = None;
        let mut oracle = None;
        let mut inputs = BTreeMap::new();
        let mut modules = BTreeMap::new();
        let mut outputs = Vec::new();
        let mut pins: BTreeMap<String, Vec<Pin>> = BTreeMap::new();

        for node in body.nodes() {
            match node.name().value() {
                "tool" => tool = Some(parse_version(&prop_str(node, "version")?)?),
                "formatter" => {
                    formatter = Some(FormatterPin {
                        name: prop_str(node, "name")?,
                        version: prop_str(node, "version")?,
                    })
                }
                "oracle" => {
                    oracle = Some(OraclePin {
                        nixpkgs_rev: prop_str(node, "nixpkgs-rev")?,
                        options_hash: prop_str(node, "options-hash")?,
                    })
                }
                "input" => {
                    inputs.insert(PathBuf::from(arg_str(node, 0)?), prop_str(node, "hash")?);
                }
                "module" => {
                    modules.insert(arg_str(node, 0)?, parse_version(&prop_str(node, "version")?)?);
                }
                "output" => outputs.push(parse_output(node)?),
                "host" => {
                    let host = arg_str(node, 0)?;
                    let mut list = Vec::new();
                    if let Some(body) = node.children() {
                        for p in body.nodes() {
                            if p.name().value() != "pin" {
                                return Err(LockError::Malformed(format!(
                                    "unexpected `{}` in host block (expected `pin`)",
                                    p.name().value()
                                )));
                            }
                            list.push(Pin {
                                package: arg_str(p, 0)?,
                                version: prop_str(p, "version")?,
                                nixpkgs_rev: prop_str(p, "nixpkgs-rev")?,
                                strategy: parse_pin_strategy(p)?,
                            });
                        }
                    }
                    list.sort_by(|a, b| a.package.cmp(&b.package));
                    pins.insert(host, list);
                }
                other => {
                    return Err(LockError::Malformed(format!("unexpected node `{other}` in lock")))
                }
            }
        }

        Ok(Lock {
            version,
            tool: tool.ok_or_else(|| LockError::Malformed("missing `tool`".into()))?,
            formatter: formatter.ok_or_else(|| LockError::Malformed("missing `formatter`".into()))?,
            oracle: oracle.ok_or_else(|| LockError::Malformed("missing `oracle`".into()))?,
            inputs,
            modules,
            outputs,
            pins,
        })
    }
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("lock version={} {{\n", self.version));
        s.push_str(&format!("    tool version=\"{}\"\n", esc(&self.tool.to_string())));
        s.push_str(&format!(
            "    formatter name=\"{}\" version=\"{}\"\n",
            esc(&self.formatter.name),
            esc(&self.formatter.version),
        ));
        s.push_str(&format!(
            "    oracle nixpkgs-rev=\"{}\" options-hash=\"{}\"\n",
            esc(&self.oracle.nixpkgs_rev),
            esc(&self.oracle.options_hash),
        ));

        s.push('\n');
        for (path, hash) in &self.inputs {
            s.push_str(&format!(
                "    input \"{}\" hash=\"{}\"\n",
                esc(&path.display().to_string()),
                esc(hash),
            ));
        }

        s.push('\n');
        for (name, version) in &self.modules {
            s.push_str(&format!(
                "    module \"{}\" version=\"{}\"\n",
                esc(name),
                esc(&version.to_string()),
            ));
        }

        for (host, list) in &self.pins {
            if list.is_empty() { continue; }
            s.push('\n');
            s.push_str(&format!("    host \"{}\" {{\n", esc(host)));
            for p in list {
                s.push_str(&format!(
                    "        pin \"{}\" version=\"{}\" nixpkgs-rev=\"{}\"",
                    esc(&p.package), esc(&p.version), esc(&p.nixpkgs_rev),
                ));
                if p.strategy == PinStrategy::Override {
                    s.push_str(" strategy=\"override\"");
                }
                s.push('\n');
            }
            s.push_str("    }\n");
        }

        for out in &self.outputs {
            s.push('\n');
            s.push_str(&format!("    output \"{}\" {{\n", esc(&out.path.display().to_string())));
            s.push_str(&format!("        hash \"{}\"\n", esc(&out.hash)));
            s.push_str(&format!("        from \"{}\"\n", esc(&out.from.display().to_string())));
            s.push_str("        modules");
            for m in &out.modules {
                s.push_str(&format!(" \"{}\"", esc(m)));
            }
            s.push('\n');
            s.push_str("    }\n");
        }

        s.push_str("}\n");
        s
    }
}

fn parse_output(node: &KdlNode) -> Result<OutputEntry, LockError> {
    let path = PathBuf::from(arg_str(node, 0)?);
    let body = node
        .children()
        .ok_or_else(|| LockError::Malformed(format!("output `{}` has no body", path.display())))?;

    let mut hash = None;
    let mut from = None;
    let mut modules = Vec::new();
    for child in body.nodes() {
        match child.name().value() {
            "hash" => hash = Some(arg_str(child, 0)?),
            "from" => from = Some(PathBuf::from(arg_str(child, 0)?)),
            "modules" => modules = all_args(child),
            other => {
                return Err(LockError::Malformed(format!("unexpected `{other}` in output body")))
            }
        }
    }

    Ok(OutputEntry {
        path,
        hash: hash.ok_or_else(|| LockError::Malformed("output missing `hash`".into()))?,
        from: from.ok_or_else(|| LockError::Malformed("output missing `from`".into()))?,
        modules,
    })
}

fn prop_str(node: &KdlNode, key: &str) -> Result<String, LockError> {
    node.get(key)
        .and_then(|v| v.as_string())
        .map(str::to_string)
        .ok_or_else(|| {
            LockError::Malformed(format!("`{}` missing string prop `{key}`", node.name().value()))
        })
}

/// The pin's `strategy` attr: absent means `CommitMix` (the default), `"override"` means
/// `Override`, anything else is malformed.
fn parse_pin_strategy(node: &KdlNode) -> Result<PinStrategy, LockError> {
    match node.get("strategy").and_then(|v| v.as_string()) {
        None => Ok(PinStrategy::CommitMix),
        Some("override") => Ok(PinStrategy::Override),
        Some(other) => Err(LockError::Malformed(format!(
            "pin has unknown strategy `{other}` (expected `override`)"
        ))),
    }
}

/// The nth positional (unnamed) argument, as a string.
fn arg_str(node: &KdlNode, idx: usize) -> Result<String, LockError> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .nth(idx)
        .and_then(|e| e.value().as_string())
        .map(str::to_string)
        .ok_or_else(|| {
            LockError::Malformed(format!("`{}` missing positional arg {idx}", node.name().value()))
        })
}

/// All positional string arguments, in source order.
fn all_args(node: &KdlNode) -> Vec<String> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .filter_map(|e| e.value().as_string().map(str::to_string))
        .collect()
}

fn parse_version(s: &str) -> Result<Version, LockError> {
    Version::parse(s).map_err(|e| LockError::Malformed(format!("bad version `{s}`: {e}")))
}

/// Minimal KDL double-quoted-string escaping. Lock values (paths, hashes, versions)
/// are simple, but escape the two characters that would break the quoting anyway.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("malformed lockfile: {0}")]
    Malformed(String),
}


#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Lock {
        let mut inputs = BTreeMap::new();
        inputs.insert(PathBuf::from("hosts/web.kdl"), "blake3:9f2c".to_string());
        inputs.insert(PathBuf::from("hosts/db.kdl"), "blake3:1b90".to_string());

        let mut modules = BTreeMap::new();
        modules.insert("host".to_string(), Version::parse("1.0.0").unwrap());
        modules.insert("postgres".to_string(), Version::parse("0.4.0").unwrap());

        Lock {
            version: 1,
            tool: Version::parse("0.3.1").unwrap(),
            formatter: FormatterPin { name: "nixfmt-rfc-style".into(), version: "0.6.0".into() },
            oracle: OraclePin { nixpkgs_rev: "a1b2c3d".into(), options_hash: "blake3:77de".into() },
            inputs,
            modules,
            outputs: vec![
                OutputEntry {
                    path: PathBuf::from("generated/hosts/web.nix"),
                    hash: "blake3:4a71".into(),
                    from: PathBuf::from("hosts/web.kdl"),
                    modules: vec!["host".into(), "web-service".into()],
                },
                OutputEntry {
                    path: PathBuf::from("generated/hosts/db.nix"),
                    hash: "blake3:c3f8".into(),
                    from: PathBuf::from("hosts/db.kdl"),
                    modules: vec!["host".into(), "postgres".into()],
                },
            ],
            pins: BTreeMap::new(),
        }
    }

    #[test]
    fn round_trips_through_render_then_parse() {
        let lock = sample();
        assert_eq!(Lock::parse(&lock.render()).expect("parse"), lock);
    }

    #[test]
    fn render_is_idempotent() {
        let once = sample().render();
        let twice = Lock::parse(&once).expect("parse").render();
        assert_eq!(once, twice);
    }

    #[test]
    fn output_with_no_modules_round_trips() {
        let mut lock = sample();
        lock.outputs = vec![OutputEntry {
            path: PathBuf::from("generated/hosts/empty.nix"),
            hash: "blake3:0000".into(),
            from: PathBuf::from("hosts/empty.kdl"),
            modules: vec![],
        }];
        assert_eq!(Lock::parse(&lock.render()).expect("parse"), lock);
    }

    #[test]
    fn pins_round_trip_and_are_deterministic() {
        let src = r#"lock version=1 {
    tool version="0.3.1"
    formatter name="nixfmt-rfc-style" version="0.6.0"
    oracle nixpkgs-rev="deadbeef" options-hash="blake3:x"
    host "laptop" {
        pin "htop" version="3.2.1" nixpkgs-rev="abc123"
    }
}
"#;
        let lock = Lock::parse(src).expect("parse");
        let pins = lock.pins.get("laptop").expect("laptop pins");
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].package, "htop");
        assert_eq!(pins[0].version, "3.2.1");
        assert_eq!(pins[0].nixpkgs_rev, "abc123");
        // Re-parsing the rendered form yields the same pins (byte-stable ordering).
        let again = Lock::parse(&lock.render()).expect("reparse");
        assert_eq!(again.pins, lock.pins);
    }

    #[test]
    fn lock_without_host_block_parses_with_no_pins() {
        let src = r#"lock version=1 {
    tool version="0.3.1"
    formatter name="nixfmt-rfc-style" version="0.6.0"
    oracle nixpkgs-rev="deadbeef" options-hash="blake3:x"
}
"#;
        let lock = Lock::parse(src).expect("parse");
        assert!(lock.pins.is_empty(), "back-compat: no host block means no pins");
    }

    #[test]
    fn pin_strategy_override_round_trips() {
        let src = r#"lock version=1 {
    tool version="0.3.1"
    formatter name="nixfmt-rfc-style" version="0.6.0"
    oracle nixpkgs-rev="deadbeef" options-hash="blake3:x"
    host "laptop" {
        pin "htop" version="3.2.1" nixpkgs-rev="abc123" strategy="override"
    }
}
"#;
        let lock = Lock::parse(src).expect("parse");
        let pins = lock.pins.get("laptop").expect("laptop pins");
        assert_eq!(pins[0].strategy, PinStrategy::Override);
        let rendered = lock.render();
        assert!(rendered.contains(
            "pin \"htop\" version=\"3.2.1\" nixpkgs-rev=\"abc123\" strategy=\"override\"\n"
        ));
        assert_eq!(Lock::parse(&rendered).expect("reparse").pins, lock.pins);
    }

    #[test]
    fn pin_strategy_absent_defaults_to_commit_mix_and_renders_without_attr() {
        let src = r#"lock version=1 {
    tool version="0.3.1"
    formatter name="nixfmt-rfc-style" version="0.6.0"
    oracle nixpkgs-rev="deadbeef" options-hash="blake3:x"
    host "laptop" {
        pin "htop" version="3.2.1" nixpkgs-rev="abc123"
    }
}
"#;
        let lock = Lock::parse(src).expect("parse");
        let pins = lock.pins.get("laptop").expect("laptop pins");
        assert_eq!(pins[0].strategy, PinStrategy::CommitMix);
        // Back-compat: no `strategy` attr in, none out.
        let rendered = lock.render();
        assert!(rendered.contains("pin \"htop\" version=\"3.2.1\" nixpkgs-rev=\"abc123\"\n"));
        assert!(!rendered.contains("strategy"));
        assert_eq!(Lock::parse(&rendered).expect("reparse").pins, lock.pins);
    }

    #[test]
    fn pin_strategy_unknown_value_is_a_parse_error() {
        let src = r#"lock version=1 {
    tool version="0.3.1"
    formatter name="nixfmt-rfc-style" version="0.6.0"
    oracle nixpkgs-rev="deadbeef" options-hash="blake3:x"
    host "laptop" {
        pin "htop" version="3.2.1" nixpkgs-rev="abc123" strategy="bogus"
    }
}
"#;
        assert!(Lock::parse(src).is_err());
    }
}
