//! knixl.lock.kdl: KDL for grep/diff friendliness. Records everything on the
//! reproducibility boundary: tool, formatter, oracle rev, and per-file hashes.
use kdl::{KdlDocument, KdlNode};
use semver::Version;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub type Hash = String; // "blake3:...."

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lock {
    pub version: u32,
    pub tool: Version,
    pub formatter: FormatterPin,
    pub oracle: OraclePin,
    pub module_sources: Vec<ModuleSourcePin>,
    pub inputs: BTreeMap<PathBuf, Hash>,
    pub modules: BTreeMap<String, Version>,
    pub outputs: Vec<OutputEntry>,
    pub pins: BTreeMap<String, Vec<Pin>>,
    pub baselines: BTreeMap<String, HostBaseline>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatterPin {
    pub name: String,
    pub version: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OraclePin {
    pub nixpkgs_rev: String,
    pub options_hash: Hash,
    pub modules: Vec<OracleModulePin>,
}

/// The oracle rev and release a host was last generated against, recorded per host so a
/// host can move to a newer nixpkgs baseline independently of the others (issue #22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostBaseline {
    pub release: String,
    pub nixpkgs_rev: String,
    pub options_hash: Hash,
    pub modules: Vec<OracleModulePin>,
}

/// A pin for an out-of-tree module source the oracle draws options from (e.g. disko,
/// impermanence): recorded so those sources are reproducible alongside nixpkgs itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleModulePin {
    pub name: String,
    pub url: String,
    pub rev: String,
    pub attr: String,
}

/// A pin for a fetched declarative module source (issue #13): the resolved source and the
/// exact bytes, so a declared module is reproducible and generate stays offline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSourcePin {
    pub name: String,
    pub url: String,
    pub rev: String,
    pub hash: Hash,
}

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
        let mut module_sources = Vec::new();
        let mut inputs = BTreeMap::new();
        let mut modules = BTreeMap::new();
        let mut outputs = Vec::new();
        let mut pins: BTreeMap<String, Vec<Pin>> = BTreeMap::new();
        let mut baselines: BTreeMap<String, HostBaseline> = BTreeMap::new();

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
                        modules: parse_oracle_modules(node)?,
                    })
                }
                "module-source" => {
                    module_sources.push(ModuleSourcePin {
                        name: arg_str(node, 0)?,
                        url: prop_str(node, "url")?,
                        rev: prop_str(node, "rev")?,
                        hash: prop_str(node, "hash")?,
                    });
                }
                "input" => {
                    inputs.insert(PathBuf::from(arg_str(node, 0)?), prop_str(node, "hash")?);
                }
                "module" => {
                    modules.insert(
                        arg_str(node, 0)?,
                        parse_version(&prop_str(node, "version")?)?,
                    );
                }
                "output" => outputs.push(parse_output(node)?),
                "host" => {
                    let host = arg_str(node, 0)?;
                    let mut list = Vec::new();
                    let mut baseline: Option<HostBaseline> = None;
                    if let Some(body) = node.children() {
                        for p in body.nodes() {
                            match p.name().value() {
                                "pin" => list.push(Pin {
                                    package: arg_str(p, 0)?,
                                    version: prop_str(p, "version")?,
                                    nixpkgs_rev: prop_str(p, "nixpkgs-rev")?,
                                    strategy: parse_pin_strategy(p)?,
                                }),
                                "baseline" => {
                                    if baseline.is_some() {
                                        return Err(LockError::Malformed(format!(
                                            "host `{host}` has more than one `baseline`"
                                        )));
                                    }
                                    baseline = Some(HostBaseline {
                                        release: prop_str(p, "release")?,
                                        nixpkgs_rev: prop_str(p, "nixpkgs-rev")?,
                                        options_hash: prop_str(p, "options-hash")?,
                                        modules: parse_oracle_modules(p)?,
                                    });
                                }
                                other => {
                                    return Err(LockError::Malformed(format!(
                                        "unexpected `{other}` in host block (expected `pin` or `baseline`)"
                                    )));
                                }
                            }
                        }
                    }
                    list.sort_by(|a, b| a.package.cmp(&b.package));
                    pins.insert(host.clone(), list);
                    if let Some(baseline) = baseline {
                        baselines.insert(host, baseline);
                    }
                }
                other => {
                    return Err(LockError::Malformed(format!(
                        "unexpected node `{other}` in lock"
                    )))
                }
            }
        }

        Ok(Lock {
            version,
            tool: tool.ok_or_else(|| LockError::Malformed("missing `tool`".into()))?,
            formatter: formatter
                .ok_or_else(|| LockError::Malformed("missing `formatter`".into()))?,
            oracle: oracle.ok_or_else(|| LockError::Malformed("missing `oracle`".into()))?,
            module_sources,
            inputs,
            modules,
            outputs,
            pins,
            baselines,
        })
    }
    pub fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("lock version={} {{\n", self.version));
        s.push_str(&format!(
            "    tool version=\"{}\"\n",
            esc(&self.tool.to_string())
        ));
        s.push_str(&format!(
            "    formatter name=\"{}\" version=\"{}\"\n",
            esc(&self.formatter.name),
            esc(&self.formatter.version),
        ));
        s.push_str(&format!(
            "    oracle nixpkgs-rev=\"{}\" options-hash=\"{}\"",
            esc(&self.oracle.nixpkgs_rev),
            esc(&self.oracle.options_hash),
        ));
        render_oracle_modules_block(&mut s, "    ", "        ", &self.oracle.modules);

        if !self.module_sources.is_empty() {
            let mut module_sources: Vec<&ModuleSourcePin> = self.module_sources.iter().collect();
            module_sources.sort_by(|a, b| a.name.cmp(&b.name));
            s.push('\n');
            for m in module_sources {
                s.push_str(&format!(
                    "    module-source \"{}\" url=\"{}\" rev=\"{}\" hash=\"{}\"\n",
                    esc(&m.name),
                    esc(&m.url),
                    esc(&m.rev),
                    esc(&m.hash),
                ));
            }
        }

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

        let mut hosts: std::collections::BTreeSet<&String> = self.baselines.keys().collect();
        hosts.extend(
            self.pins
                .iter()
                .filter(|(_, list)| !list.is_empty())
                .map(|(host, _)| host),
        );
        for host in hosts {
            s.push('\n');
            s.push_str(&format!("    host \"{}\" {{\n", esc(host)));
            if let Some(baseline) = self.baselines.get(host) {
                s.push_str(&format!(
                    "        baseline release=\"{}\" nixpkgs-rev=\"{}\" options-hash=\"{}\"",
                    esc(&baseline.release),
                    esc(&baseline.nixpkgs_rev),
                    esc(&baseline.options_hash),
                ));
                render_oracle_modules_block(&mut s, "        ", "            ", &baseline.modules);
            }
            for p in self.pins.get(host).into_iter().flatten() {
                s.push_str(&format!(
                    "        pin \"{}\" version=\"{}\" nixpkgs-rev=\"{}\"",
                    esc(&p.package),
                    esc(&p.version),
                    esc(&p.nixpkgs_rev),
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
            s.push_str(&format!(
                "    output \"{}\" {{\n",
                esc(&out.path.display().to_string())
            ));
            s.push_str(&format!("        hash \"{}\"\n", esc(&out.hash)));
            s.push_str(&format!(
                "        from \"{}\"\n",
                esc(&out.from.display().to_string())
            ));
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

/// Closes the still-open `oracle`/`baseline` line (its trailing `\n` not yet written) with
/// a `{ .. }` block of `oracle-module` children when `modules` is non-empty, one per pin in
/// order, each indented by `child_indent`; the block itself closes at `parent_indent`. KDL
/// nests children only inside braces, so this is the difference between a real child and a
/// bare sibling line. Empty `modules` just closes the line with `\n` and no braces at all,
/// so a lock with no module pins renders exactly as before this field existed.
fn render_oracle_modules_block(
    s: &mut String,
    parent_indent: &str,
    child_indent: &str,
    modules: &[OracleModulePin],
) {
    if modules.is_empty() {
        s.push('\n');
        return;
    }
    s.push_str(" {\n");
    for m in modules {
        s.push_str(&format!(
            "{child_indent}oracle-module name=\"{}\" url=\"{}\" rev=\"{}\" attr=\"{}\"\n",
            esc(&m.name),
            esc(&m.url),
            esc(&m.rev),
            esc(&m.attr),
        ));
    }
    s.push_str(&format!("{parent_indent}}}\n"));
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
                return Err(LockError::Malformed(format!(
                    "unexpected `{other}` in output body"
                )))
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

/// The `oracle-module` children of an `oracle` or `baseline` node, in source order. Absent
/// children (no `oracle-module` lines) yields an empty vec.
fn parse_oracle_modules(node: &KdlNode) -> Result<Vec<OracleModulePin>, LockError> {
    let Some(body) = node.children() else {
        return Ok(Vec::new());
    };
    body.nodes()
        .iter()
        .filter(|child| child.name().value() == "oracle-module")
        .map(|child| {
            let name = arg_str(child, 0).or_else(|_| prop_str(child, "name"))?;
            Ok(OracleModulePin {
                name,
                url: prop_str(child, "url")?,
                rev: prop_str(child, "rev")?,
                attr: prop_str(child, "attr")?,
            })
        })
        .collect()
}

fn prop_str(node: &KdlNode, key: &str) -> Result<String, LockError> {
    node.get(key)
        .and_then(|v| v.as_string())
        .map(str::to_string)
        .ok_or_else(|| {
            LockError::Malformed(format!(
                "`{}` missing string prop `{key}`",
                node.name().value()
            ))
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
            LockError::Malformed(format!(
                "`{}` missing positional arg {idx}",
                node.name().value()
            ))
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
            formatter: FormatterPin {
                name: "nixfmt-rfc-style".into(),
                version: "0.6.0".into(),
            },
            oracle: OraclePin {
                nixpkgs_rev: "a1b2c3d".into(),
                options_hash: "blake3:77de".into(),
                modules: vec![],
            },
            module_sources: vec![],
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
            baselines: BTreeMap::new(),
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
        assert!(
            lock.pins.is_empty(),
            "back-compat: no host block means no pins"
        );
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

    #[test]
    fn baseline_round_trips_before_pin_and_lands_in_baselines_map() {
        let src = r#"lock version=1 {
    tool version="0.3.1"
    formatter name="nixfmt-rfc-style" version="0.6.0"
    oracle nixpkgs-rev="deadbeef" options-hash="blake3:x"
    host "web" {
        baseline release="25.05" nixpkgs-rev="abc" options-hash="blake3:x"
        pin "htop" version="3.2.1" nixpkgs-rev="abc"
    }
}
"#;
        let lock = Lock::parse(src).expect("parse");
        let baseline = lock.baselines.get("web").expect("web baseline");
        assert_eq!(baseline.release, "25.05");
        assert_eq!(baseline.nixpkgs_rev, "abc");
        assert_eq!(baseline.options_hash, "blake3:x");
        let pins = lock.pins.get("web").expect("web pins");
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].package, "htop");

        let rendered = lock.render();
        let baseline_idx = rendered.find("baseline").expect("baseline line present");
        let pin_idx = rendered.find("pin \"htop\"").expect("pin line present");
        assert!(baseline_idx < pin_idx, "baseline must render before pin");
        assert_eq!(Lock::parse(&rendered).expect("reparse"), lock);
    }

    #[test]
    fn host_with_only_pins_has_no_baseline_and_renders_unchanged() {
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
        assert!(
            lock.baselines.is_empty(),
            "back-compat: no baseline line means no baselines"
        );
        // Byte-for-byte back-compat: a host with no baseline renders exactly the same
        // host block as before this field existed, with no `baseline` line at all.
        let rendered = lock.render();
        assert!(!rendered.contains("baseline"));
        assert!(rendered.contains(
            "    host \"laptop\" {\n        pin \"htop\" version=\"3.2.1\" nixpkgs-rev=\"abc123\"\n    }\n"
        ));
        assert_eq!(Lock::parse(&rendered).expect("reparse"), lock);
    }

    #[test]
    fn two_baselines_in_one_host_is_a_parse_error() {
        let src = r#"lock version=1 {
    tool version="0.3.1"
    formatter name="nixfmt-rfc-style" version="0.6.0"
    oracle nixpkgs-rev="deadbeef" options-hash="blake3:x"
    host "web" {
        baseline release="25.05" nixpkgs-rev="abc" options-hash="blake3:x"
        baseline release="25.11" nixpkgs-rev="def" options-hash="blake3:y"
    }
}
"#;
        assert!(Lock::parse(src).is_err());
    }

    #[test]
    fn oracle_module_pins_round_trip() {
        let mut lock = sample();
        lock.oracle.modules = vec![OracleModulePin {
            name: "disko".into(),
            url: "https://github.com/nix-community/disko".into(),
            rev: "abc".into(),
            attr: "default".into(),
        }];
        lock.baselines.insert(
            "web".to_string(),
            HostBaseline {
                release: "25.05".into(),
                nixpkgs_rev: "abc123".into(),
                options_hash: "blake3:opts".into(),
                modules: vec![OracleModulePin {
                    name: "impermanence".into(),
                    url: "https://github.com/nix-community/impermanence".into(),
                    rev: "def".into(),
                    attr: "nixosModules.impermanence".into(),
                }],
            },
        );
        // A rendered host block, whether reached via `baselines` or `pins`, always parses
        // back with a (possibly empty) `pins` entry for that host.
        lock.pins.insert("web".to_string(), vec![]);

        let text = lock.render();
        let back = Lock::parse(&text).expect("parse");
        assert_eq!(back, lock);
        assert!(text.contains("oracle-module name=\"disko\""));
        assert!(text.contains("oracle-module name=\"impermanence\""));
    }

    #[test]
    fn a_lock_without_module_pins_renders_unchanged() {
        let text = sample().render();
        assert!(!text.contains("oracle-module"));
    }

    #[test]
    fn module_source_pins_round_trip() {
        let mut lock = sample();
        lock.module_sources = vec![ModuleSourcePin {
            name: "web-service".into(),
            url: "https://example.com/modules/web-service.tar.gz".into(),
            rev: "abc123".into(),
            hash: "blake3:feed".into(),
        }];

        let text = lock.render();
        let back = Lock::parse(&text).expect("parse");
        assert_eq!(back, lock);
        assert!(text.contains(
            "module-source \"web-service\" url=\"https://example.com/modules/web-service.tar.gz\" rev=\"abc123\" hash=\"blake3:feed\""
        ));
    }

    #[test]
    fn module_source_pins_are_sorted_by_name_on_render() {
        let mut lock = sample();
        lock.module_sources = vec![
            ModuleSourcePin {
                name: "zeta".into(),
                url: "https://example.com/zeta".into(),
                rev: "z1".into(),
                hash: "blake3:zzz".into(),
            },
            ModuleSourcePin {
                name: "alpha".into(),
                url: "https://example.com/alpha".into(),
                rev: "a1".into(),
                hash: "blake3:aaa".into(),
            },
        ];

        let text = lock.render();
        let alpha_idx = text.find("module-source \"alpha\"").expect("alpha present");
        let zeta_idx = text.find("module-source \"zeta\"").expect("zeta present");
        assert!(
            alpha_idx < zeta_idx,
            "module sources must render sorted by name"
        );
    }

    #[test]
    fn a_lock_without_module_sources_renders_unchanged() {
        let text = sample().render();
        assert!(!text.contains("module-source"));
    }

    #[test]
    fn unknown_child_in_host_block_is_a_parse_error() {
        let src = r#"lock version=1 {
    tool version="0.3.1"
    formatter name="nixfmt-rfc-style" version="0.6.0"
    oracle nixpkgs-rev="deadbeef" options-hash="blake3:x"
    host "web" {
        mystery "wat"
    }
}
"#;
        assert!(Lock::parse(src).is_err());
    }
}
