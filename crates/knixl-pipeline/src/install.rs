//! Support for `knixl install`: enumerate hosts, pick the target, and splice a `package`
//! node into a host's KDL without disturbing the rest of the file.

use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlNode};

/// A host as seen by `install`: its name, whether it is marked default, and its file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    pub name: String,
    pub default: bool,
    pub path: PathBuf,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SelectError {
    #[error("no hosts found under hosts/")]
    NoHosts,
    #[error("no host named `{0}`")]
    Unknown(String),
    #[error("more than one host is marked default: {}", .0.join(", "))]
    ManyDefaults(Vec<String>),
    #[error("several hosts exist; pass --host <name> or mark one default=#true: {}", .0.join(", "))]
    Ambiguous(Vec<String>),
}

/// Choose the target host. Order: explicit `--host`, then the `default` host, then the sole
/// host, else an error.
pub fn select_host<'a>(
    hosts: &'a [HostInfo],
    requested: Option<&str>,
) -> Result<&'a HostInfo, SelectError> {
    if hosts.is_empty() {
        return Err(SelectError::NoHosts);
    }
    if let Some(name) = requested {
        return hosts.iter().find(|h| h.name == name).ok_or_else(|| SelectError::Unknown(name.into()));
    }
    let defaults: Vec<&HostInfo> = hosts.iter().filter(|h| h.default).collect();
    match defaults.len() {
        1 => Ok(defaults[0]),
        0 => {
            if hosts.len() == 1 {
                Ok(&hosts[0])
            } else {
                Err(SelectError::Ambiguous(hosts.iter().map(|h| h.name.clone()).collect()))
            }
        }
        _ => Err(SelectError::ManyDefaults(defaults.iter().map(|h| h.name.clone()).collect())),
    }
}

/// Append `package "<pkg>"` as a child of the single top-level `host` node in `src`,
/// preserving the rest of the file's formatting. Returns the new source, or `None` if the
/// host already declares that package (idempotent).
pub fn add_package(src: &str, pkg: &str) -> Result<Option<String>, String> {
    let doc: KdlDocument = src.parse().map_err(|e: kdl::KdlError| e.to_string())?;
    let host = doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "host")
        .ok_or_else(|| "no `host` node to add a package to".to_string())?;

    if has_package(host, pkg) {
        return Ok(None);
    }

    // Splice at the text level, keyed off the host node's span, so every other byte
    // (comments, spacing) is preserved exactly. Insert a `package` line just before the
    // host block's closing brace.
    let start = host.span().offset();
    let end = (start + host.span().len()).min(src.len());
    let host_text = &src[start..end];
    let close = start
        + host_text
            .rfind('}')
            .ok_or_else(|| "host has no children block to extend".to_string())?;
    let line_start = src[..close].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let indent = detect_indent(src);

    let insertion = format!("{indent}package \"{pkg}\"\n");
    let mut out = String::with_capacity(src.len() + insertion.len());
    out.push_str(&src[..line_start]);
    out.push_str(&insertion);
    out.push_str(&src[line_start..]);
    Ok(Some(out))
}

/// The indentation of the first indented, non-empty line, defaulting to four spaces.
fn detect_indent(src: &str) -> String {
    src.lines()
        .find_map(|l| {
            let trimmed = l.trim_start_matches([' ', '\t']);
            let n = l.len() - trimmed.len();
            (n > 0 && !trimmed.is_empty()).then(|| l[..n].to_string())
        })
        .unwrap_or_else(|| "    ".to_string())
}

/// Read `root/hosts/*.kdl`, returning one `HostInfo` per file (name = the host node's first
/// argument, default = its `default` prop).
pub fn list_hosts(root: &Path) -> std::io::Result<Vec<HostInfo>> {
    let dir = root.join("hosts");
    let mut hosts = Vec::new();
    if !dir.is_dir() {
        return Ok(hosts);
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "kdl"))
        .collect();
    paths.sort();
    for path in paths {
        let src = std::fs::read_to_string(&path)?;
        if let Some(info) = host_info(&src, &path) {
            hosts.push(info);
        }
    }
    Ok(hosts)
}

fn host_info(src: &str, path: &Path) -> Option<HostInfo> {
    let doc: KdlDocument = src.parse().ok()?;
    let node = doc.nodes().iter().find(|n| n.name().value() == "host")?;
    let name = node
        .entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
        .map(str::to_string)?;
    let default = node.get("default").and_then(|v| v.as_bool()).unwrap_or(false);
    Some(HostInfo { name, default, path: path.to_path_buf() })
}

/// True if `host` already has a `package "<pkg>"` child.
fn has_package(host: &KdlNode, pkg: &str) -> bool {
    host.children().is_some_and(|doc| {
        doc.nodes().iter().any(|n| {
            n.name().value() == "package"
                && n.entries().iter().any(|e| e.name().is_none() && e.value().as_string() == Some(pkg))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, default: bool) -> HostInfo {
        HostInfo { name: name.into(), default, path: PathBuf::from(format!("hosts/{name}.kdl")) }
    }

    #[test]
    fn explicit_host_wins() {
        let hs = [host("web", false), host("db", true)];
        assert_eq!(select_host(&hs, Some("web")).unwrap().name, "web");
    }

    #[test]
    fn unknown_explicit_host_errors() {
        let hs = [host("web", false)];
        assert_eq!(select_host(&hs, Some("nope")), Err(SelectError::Unknown("nope".into())));
    }

    #[test]
    fn default_host_used_when_no_flag() {
        let hs = [host("web", false), host("db", true)];
        assert_eq!(select_host(&hs, None).unwrap().name, "db");
    }

    #[test]
    fn sole_host_used_when_no_default_no_flag() {
        let hs = [host("only", false)];
        assert_eq!(select_host(&hs, None).unwrap().name, "only");
    }

    #[test]
    fn several_hosts_no_default_is_ambiguous() {
        let hs = [host("web", false), host("db", false)];
        assert!(matches!(select_host(&hs, None), Err(SelectError::Ambiguous(_))));
    }

    #[test]
    fn two_defaults_is_an_error() {
        let hs = [host("web", true), host("db", true)];
        assert!(matches!(select_host(&hs, None), Err(SelectError::ManyDefaults(_))));
    }

    #[test]
    fn add_package_appends_under_host() {
        let src = "host \"web\" {\n    system \"x86_64-linux\"\n}\n";
        let out = add_package(src, "ripgrep").unwrap().expect("edit produced");
        assert!(out.contains("package"), "package node added: {out}");
        assert!(out.contains("ripgrep"), "package name present: {out}");
        assert!(out.contains("system \"x86_64-linux\""), "existing content kept: {out}");
        // The edit round-trips as valid KDL with the new package present.
        let doc: KdlDocument = out.parse().expect("valid kdl");
        let host = doc.nodes().iter().find(|n| n.name().value() == "host").unwrap();
        assert!(has_package(host, "ripgrep"));
    }

    #[test]
    fn add_package_is_idempotent() {
        let src = "host \"web\" {\n    system \"x86_64-linux\"\n    package \"ripgrep\"\n}\n";
        assert_eq!(add_package(src, "ripgrep").unwrap(), None, "already present is a no-op");
    }
}
