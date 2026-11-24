//! The update report behind `knixl updates`: what moving each host to the newest commit on its
//! nixpkgs release would change, from two evaluated (never built) system derivations. Package
//! versions come from the derivation closures, and security fixes from vulnix scans of both.
//! Everything here is pure; the CLI does the evaluating and scanning.

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Fetched sources and patches, which sit in a build closure beside the package they feed and
/// would otherwise show up as packages of their own (`CVE-2014-8139.diff`, `curl-8.20.0.tar.xz`).
const SOURCE_SUFFIXES: &[&str] = &[
    ".patch", ".diff", ".tar.xz", ".tar.gz", ".tar.bz2", ".tar.zst", ".tgz", ".tar", ".zip",
    ".cabal", ".gz", ".xz", ".bz2", ".zst",
];

/// Split a derivation path or name into `(pname, version)` the way `builtins.parseDrvName`
/// does: the version starts at the first `-` followed by a digit. `None` for an unversioned
/// derivation (a hook, a config file) or a fetched source or patch, which aren't packages.
pub fn parse_drv_name(path: &str) -> Option<(String, String)> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let base = base.strip_suffix(".drv").unwrap_or(base);
    // A store path's basename starts with a 32-character hash and a dash.
    let name = match base.split_once('-') {
        Some((hash, rest)) if hash.len() == 32 => rest,
        _ => base,
    };
    if SOURCE_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return None;
    }
    let bytes = name.as_bytes();
    let at = (0..bytes.len().saturating_sub(1))
        .find(|&i| bytes[i] == b'-' && bytes[i + 1].is_ascii_digit())?;
    Some((name[..at].to_string(), name[at + 1..].to_string()))
}

/// How big a version change is, judged by the first component that differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    Major,
    Minor,
    Patch,
    /// The versions differ somewhere that isn't a plain number (a date, a suffix, a scheme).
    Other,
    Downgrade,
}

impl Bump {
    pub fn label(self) -> &'static str {
        match self {
            Bump::Major => "major",
            Bump::Minor => "minor",
            Bump::Patch => "patch",
            Bump::Other => "other",
            Bump::Downgrade => "downgrade",
        }
    }
}

/// Components as Nix's `splitVersion` makes them: split on `.` and `-`, and between a run of
/// digits and a run of anything else.
fn components(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in v.chars() {
        if c == '.' || c == '-' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some(last) = cur.chars().last() {
            if last.is_ascii_digit() != c.is_ascii_digit() {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Classify `old` -> `new`. Numeric components compare as numbers; the first difference
/// decides: position 0 is major, 1 minor, later patch, and a non-numeric difference is other.
pub fn classify(old: &str, new: &str) -> Bump {
    let (a, b) = (components(old), components(new));
    for i in 0..a.len().max(b.len()) {
        match (a.get(i), b.get(i)) {
            (Some(x), Some(y)) if x == y => continue,
            (Some(x), Some(y)) => {
                return match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) if y < x => Bump::Downgrade,
                    (Ok(_), Ok(_)) => match i {
                        0 => Bump::Major,
                        1 => Bump::Minor,
                        _ => Bump::Patch,
                    },
                    _ => Bump::Other,
                };
            }
            // 1.2 -> 1.2.1 adds a component: a patch at that depth, a minor or major above it.
            (None, Some(_)) => {
                return match i {
                    0 => Bump::Major,
                    1 => Bump::Minor,
                    _ => Bump::Patch,
                }
            }
            (Some(_), None) => return Bump::Downgrade,
            (None, None) => break,
        }
    }
    Bump::Other
}

/// One package whose versions differ between the two closures.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionChange {
    pub pname: String,
    pub old: Vec<String>,
    pub new: Vec<String>,
    pub bump: Bump,
}

/// What changed between two closures, by package name.
#[derive(Debug, Default, PartialEq)]
pub struct ClosureDiff {
    pub changed: Vec<VersionChange>,
    pub added: Vec<(String, String)>,
    pub removed: Vec<(String, String)>,
}

fn versions_by_pname(paths: &[String]) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for p in paths {
        if let Some((pname, version)) = parse_drv_name(p) {
            out.entry(pname).or_default().insert(version);
        }
    }
    out
}

/// Compare two derivation closures (`nix-store -qR <drv>` output). A package present on both
/// sides with different versions is a change, classified by its highest version on each side.
pub fn diff_closures(old: &[String], new: &[String]) -> ClosureDiff {
    let (a, b) = (versions_by_pname(old), versions_by_pname(new));
    let mut diff = ClosureDiff::default();
    for (pname, olds) in &a {
        match b.get(pname) {
            Some(news) if news != olds => {
                let hi_old = olds.iter().max_by(|x, y| cmp_versions(x, y)).unwrap();
                let hi_new = news.iter().max_by(|x, y| cmp_versions(x, y)).unwrap();
                diff.changed.push(VersionChange {
                    pname: pname.clone(),
                    old: olds.iter().cloned().collect(),
                    new: news.iter().cloned().collect(),
                    bump: if hi_old == hi_new {
                        Bump::Other
                    } else {
                        classify(hi_old, hi_new)
                    },
                });
            }
            Some(_) => {}
            None => diff
                .removed
                .extend(olds.iter().map(|v| (pname.clone(), v.clone()))),
        }
    }
    for (pname, news) in &b {
        if !a.contains_key(pname) {
            diff.added
                .extend(news.iter().map(|v| (pname.clone(), v.clone())));
        }
    }
    diff
}

fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
    match classify(a, b) {
        Bump::Downgrade => std::cmp::Ordering::Greater,
        _ if a == b => std::cmp::Ordering::Equal,
        _ => std::cmp::Ordering::Less,
    }
}

/// One package vulnix flags, with each CVE and its CVSS v3 base score when it has one.
#[derive(Debug, Clone, PartialEq)]
pub struct Vuln {
    pub pname: String,
    pub version: String,
    pub cves: Vec<(String, Option<f64>)>,
}

/// Parse `vulnix --json` output (a list of affected derivations). Whitelisted CVEs are dropped.
pub fn parse_vulnix(text: &str) -> Result<Vec<Vuln>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("vulnix output: {e}"))?;
    let items = v.as_array().ok_or("vulnix output: expected a list")?;
    let mut out = Vec::new();
    for item in items {
        let s = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or_default();
        let whitelisted: BTreeSet<&str> = item
            .get("whitelisted")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let scores = item.get("cvssv3_basescore");
        let cves: Vec<(String, Option<f64>)> = item
            .get("affected_by")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|c| !whitelisted.contains(c))
            .map(|c| {
                let score = scores.and_then(|s| s.get(c)).and_then(Value::as_f64);
                (c.to_string(), score)
            })
            .collect();
        if !cves.is_empty() {
            out.push(Vuln {
                pname: s("pname").to_string(),
                version: s("version").to_string(),
                cves,
            });
        }
    }
    Ok(out)
}

/// CVEs flagged on a package before the refresh and gone after it.
#[derive(Debug, Clone, PartialEq)]
pub struct SecurityFix {
    pub pname: String,
    pub old_version: String,
    /// `None` when the package left the closure altogether.
    pub new_version: Option<String>,
    pub cves: Vec<(String, Option<f64>)>,
}

/// The security side of the report: what the refresh fixes, and what stays flagged after it.
#[derive(Debug, Default, PartialEq)]
pub struct Security {
    pub fixed: Vec<SecurityFix>,
    pub remaining: Vec<Vuln>,
}

/// Compare the two scans by package name: a CVE flagged before and not after is fixed.
pub fn security(old: &[Vuln], new: &[Vuln], diff: &ClosureDiff) -> Security {
    let after: BTreeMap<&str, BTreeSet<&str>> = new.iter().fold(BTreeMap::new(), |mut m, v| {
        m.entry(v.pname.as_str())
            .or_default()
            .extend(v.cves.iter().map(|(c, _)| c.as_str()));
        m
    });
    let new_version = |pname: &str| -> Option<String> {
        diff.changed
            .iter()
            .find(|c| c.pname == pname)
            .and_then(|c| c.new.last().cloned())
            .or_else(|| {
                // Same version, CVE gone: nixpkgs patched it in place.
                (!diff.removed.iter().any(|(p, _)| p == pname)).then(|| String::from("patched"))
            })
    };
    let mut fixed = Vec::new();
    for v in old {
        let still: Option<&BTreeSet<&str>> = after.get(v.pname.as_str());
        let gone: Vec<(String, Option<f64>)> = v
            .cves
            .iter()
            .filter(|(c, _)| still.is_none_or(|s| !s.contains(c.as_str())))
            .cloned()
            .collect();
        if !gone.is_empty() {
            fixed.push(SecurityFix {
                pname: v.pname.clone(),
                old_version: v.version.clone(),
                new_version: new_version(&v.pname),
                cves: gone,
            });
        }
    }
    fixed.sort_by(|a, b| max_score(&b.cves).total_cmp(&max_score(&a.cves)));
    let mut remaining = new.to_vec();
    remaining.sort_by(|a, b| max_score(&b.cves).total_cmp(&max_score(&a.cves)));
    Security { fixed, remaining }
}

fn max_score(cves: &[(String, Option<f64>)]) -> f64 {
    cves.iter().filter_map(|(_, s)| *s).fold(0.0, f64::max)
}

/// The report for one host.
#[derive(Debug)]
pub struct HostReport {
    pub host: String,
    pub release: String,
    pub old_rev: String,
    pub new_rev: String,
    pub diff: ClosureDiff,
    /// `None` when vulnix wasn't available, so security wasn't checked.
    pub security: Option<Security>,
    /// The host's own `package` nodes (unpinned, not `from=`), by attribute name: the
    /// changes among these can be taken one at a time with a version pin.
    pub direct: Vec<String>,
}

/// The direct packages whose version changes, as (attribute, new version): each can be taken
/// alone with `knixl install <attr>@<version>`, leaving the rest of nixpkgs where it is.
pub fn direct_updates(r: &HostReport) -> Vec<(String, String)> {
    r.direct
        .iter()
        .filter_map(|attr| {
            let pname = attr.rsplit('.').next().unwrap_or(attr);
            let change = r.diff.changed.iter().find(|c| c.pname == pname)?;
            Some((attr.clone(), change.new.last()?.clone()))
        })
        .collect()
}

fn short(rev: &str) -> &str {
    &rev[..rev.len().min(7)]
}

/// Severity as a rank for comparisons: unscored 0, low 1, medium 2, high 3, critical 4.
pub fn severity_rank(score: Option<f64>) -> u8 {
    match severity(score) {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// The rank of a named severity floor (`low`, `medium`, `high`, `critical`); unscored CVEs
/// only pass the lowest floor.
pub fn severity_floor(name: &str) -> u8 {
    match name {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        _ => 0,
    }
}

fn severity(score: Option<f64>) -> &'static str {
    match score {
        Some(s) if s >= 9.0 => "critical",
        Some(s) if s >= 7.0 => "high",
        Some(s) if s >= 4.0 => "medium",
        Some(_) => "low",
        None => "unscored",
    }
}

fn cve_list(cves: &[(String, Option<f64>)]) -> String {
    cves.iter()
        .map(|(c, s)| match s {
            Some(s) => format!("{c} ({s}, {})", severity(Some(*s))),
            None => c.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The markdown report, for a PR body or a terminal.
pub fn render_markdown(reports: &[HostReport]) -> String {
    let mut s = String::from("# Software updates\n\n");
    if reports.is_empty() {
        s.push_str("Every host is on the newest commit of its nixpkgs release.\n");
        return s;
    }
    for r in reports {
        s.push_str(&format!(
            "## {}\n\nnixpkgs {}: [`{}` to `{}`](https://github.com/NixOS/nixpkgs/compare/{}...{})\n\n",
            r.host,
            r.release,
            short(&r.old_rev),
            short(&r.new_rev),
            r.old_rev,
            r.new_rev
        ));
        let count = |b: Bump| r.diff.changed.iter().filter(|c| c.bump == b).count();
        match &r.security {
            Some(sec) => {
                let n: usize = sec.fixed.iter().map(|f| f.cves.len()).sum();
                s.push_str(&format!(
                    "**{n} CVE{} fixed** in {} package{}. ",
                    if n == 1 { "" } else { "s" },
                    sec.fixed.len(),
                    if sec.fixed.len() == 1 { "" } else { "s" }
                ));
            }
            None => s.push_str("**Security not checked** (vulnix isn't available). "),
        }
        s.push_str(&format!(
            "{} version changes: {} major, {} minor, {} patch, {} other, {} downgrade.\n\n",
            r.diff.changed.len(),
            count(Bump::Major),
            count(Bump::Minor),
            count(Bump::Patch),
            count(Bump::Other),
            count(Bump::Downgrade)
        ));
        let direct = direct_updates(r);
        if !direct.is_empty() {
            s.push_str("### Updating packages one at a time\n\nThese are installed directly on the host, so each can be pinned to its new version without taking the rest of the update:\n\n");
            for (attr, version) in &direct {
                s.push_str(&format!(
                    "- `knixl install --host {} {attr}@{version}`\n",
                    r.host
                ));
            }
            s.push('\n');
        }
        if let Some(sec) = &r.security {
            if !sec.fixed.is_empty() {
                s.push_str("### Security fixes\n\n| Package | From | To | Fixes |\n| --- | --- | --- | --- |\n");
                for f in &sec.fixed {
                    s.push_str(&format!(
                        "| {} | {} | {} | {} |\n",
                        f.pname,
                        f.old_version,
                        f.new_version.as_deref().unwrap_or("removed"),
                        cve_list(&f.cves)
                    ));
                }
                s.push('\n');
            }
        }
        for b in [
            Bump::Major,
            Bump::Minor,
            Bump::Patch,
            Bump::Other,
            Bump::Downgrade,
        ] {
            let rows: Vec<&VersionChange> = r.diff.changed.iter().filter(|c| c.bump == b).collect();
            if rows.is_empty() {
                continue;
            }
            s.push_str(&format!(
                "### {} updates ({})\n\n| Package | From | To |\n| --- | --- | --- |\n",
                capitalise(b.label()),
                rows.len()
            ));
            for c in rows {
                s.push_str(&format!(
                    "| {} | {} | {} |\n",
                    c.pname,
                    c.old.join(", "),
                    c.new.join(", ")
                ));
            }
            s.push('\n');
        }
        if !r.diff.added.is_empty() || !r.diff.removed.is_empty() {
            s.push_str(&format!(
                "{} package{} added and {} removed.\n\n",
                r.diff.added.len(),
                if r.diff.added.len() == 1 { "" } else { "s" },
                r.diff.removed.len()
            ));
        }
        if let Some(sec) = &r.security {
            if !sec.remaining.is_empty() {
                s.push_str(&format!(
                    "<details><summary>Still flagged after the update ({} packages)</summary>\n\nvulnix matches CVEs by package name across the whole build closure, so some of these are false positives.\n\n| Package | Version | CVEs |\n| --- | --- | --- |\n",
                    sec.remaining.len()
                ));
                for v in &sec.remaining {
                    s.push_str(&format!(
                        "| {} | {} | {} |\n",
                        v.pname,
                        v.version,
                        cve_list(&v.cves)
                    ));
                }
                s.push_str("\n</details>\n\n");
            }
        }
    }
    s
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn cves_json(cves: &[(String, Option<f64>)]) -> Value {
    Value::Array(
        cves.iter()
            .map(|(c, s)| json!({ "id": c, "score": s, "severity": severity(*s) }))
            .collect(),
    )
}

/// The machine-readable report, for CI to branch on.
pub fn to_json(reports: &[HostReport]) -> Value {
    let hosts: Vec<Value> = reports
        .iter()
        .map(|r| {
            json!({
                "host": r.host,
                "release": r.release,
                "old_rev": r.old_rev,
                "new_rev": r.new_rev,
                "changes": r.diff.changed.iter().map(|c| json!({
                    "package": c.pname, "old": c.old, "new": c.new, "bump": c.bump.label(),
                })).collect::<Vec<_>>(),
                "added": r.diff.added.iter().map(|(p, v)| json!({"package": p, "version": v})).collect::<Vec<_>>(),
                "removed": r.diff.removed.iter().map(|(p, v)| json!({"package": p, "version": v})).collect::<Vec<_>>(),
                "direct": direct_updates(r).iter().map(|(p, v)| json!({"package": p, "version": v})).collect::<Vec<_>>(),
                "security": r.security.as_ref().map(|sec| json!({
                    "fixed": sec.fixed.iter().map(|f| json!({
                        "package": f.pname, "old": f.old_version, "new": f.new_version, "cves": cves_json(&f.cves),
                    })).collect::<Vec<_>>(),
                    "remaining": sec.remaining.iter().map(|v| json!({
                        "package": v.pname, "version": v.version, "cves": cves_json(&v.cves),
                    })).collect::<Vec<_>>(),
                })),
            })
        })
        .collect();
    let security_fixes: usize = reports
        .iter()
        .filter_map(|r| r.security.as_ref())
        .map(|s| s.fixed.iter().map(|f| f.cves.len()).sum::<usize>())
        .sum();
    json!({ "updates": !reports.is_empty(), "security_fixes": security_fixes, "hosts": hosts })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_drv_names_like_parse_drv_name() {
        assert_eq!(
            parse_drv_name("/nix/store/r63z16z566fxj0xb6zg2p0i332sx85jl-openssl-3.0.13.drv"),
            Some(("openssl".into(), "3.0.13".into()))
        );
        assert_eq!(
            parse_drv_name(
                "/nix/store/r63z16z566fxj0xb6zg2p0i332sx85jl-python3.12-requests-2.31.0.drv"
            ),
            Some(("python3.12-requests".into(), "2.31.0".into()))
        );
        assert_eq!(
            parse_drv_name("/nix/store/r63z16z566fxj0xb6zg2p0i332sx85jl-source.drv"),
            None
        );
        for source in [
            "curl-8.20.0.tar.xz",
            "CVE-2014-8139.diff",
            "async-2.2.5-r3.cabal",
        ] {
            assert_eq!(
                parse_drv_name(&format!("{}-{source}.drv", "a".repeat(32))),
                None
            );
        }
    }

    #[test]
    fn classifies_bumps() {
        assert_eq!(classify("3.0.13", "3.0.14"), Bump::Patch);
        assert_eq!(classify("3.0.13", "3.1.0"), Bump::Minor);
        assert_eq!(classify("2.9", "3.0"), Bump::Major);
        assert_eq!(classify("1.10", "1.9"), Bump::Downgrade);
        assert_eq!(classify("1.2", "1.2.1"), Bump::Patch);
        assert_eq!(
            classify("1.0-unstable-2025-05-01", "1.0-unstable-2025-06-01"),
            Bump::Patch
        );
        assert_eq!(classify("1.0rc1", "1.0rc2"), Bump::Patch);
        assert_eq!(classify("1.0a", "1.0b"), Bump::Other);
    }

    fn drv(name: &str) -> String {
        format!("/nix/store/{}-{name}.drv", "a".repeat(32))
    }

    #[test]
    fn diffs_closures_by_package() {
        let old = vec![
            drv("openssl-3.0.13"),
            drv("curl-8.4.0"),
            drv("gone-1.0"),
            drv("source"),
        ];
        let new = vec![drv("openssl-3.0.14"), drv("curl-8.4.0"), drv("fresh-2.0")];
        let d = diff_closures(&old, &new);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].pname, "openssl");
        assert_eq!(d.changed[0].bump, Bump::Patch);
        assert_eq!(d.added, vec![("fresh".to_string(), "2.0".to_string())]);
        assert_eq!(d.removed, vec![("gone".to_string(), "1.0".to_string())]);
    }

    const OLD_SCAN: &str = r#"[
      {"pname":"openssl","version":"3.0.13","affected_by":["CVE-2024-1","CVE-2024-2"],"whitelisted":[],"cvssv3_basescore":{"CVE-2024-1":9.8}},
      {"pname":"Diff","version":"1.0.2","affected_by":["CVE-2024-3"],"whitelisted":["CVE-2024-3"],"cvssv3_basescore":{}}
    ]"#;
    const NEW_SCAN: &str = r#"[
      {"pname":"openssl","version":"3.0.14","affected_by":["CVE-2024-2"],"whitelisted":[],"cvssv3_basescore":{}}
    ]"#;

    #[test]
    fn a_cve_gone_after_the_refresh_is_a_fix() {
        let (old, new) = (
            parse_vulnix(OLD_SCAN).unwrap(),
            parse_vulnix(NEW_SCAN).unwrap(),
        );
        assert_eq!(old.len(), 1, "fully whitelisted entries drop out");
        let diff = diff_closures(&[drv("openssl-3.0.13")], &[drv("openssl-3.0.14")]);
        let sec = security(&old, &new, &diff);
        assert_eq!(sec.fixed.len(), 1);
        assert_eq!(
            sec.fixed[0].cves,
            vec![("CVE-2024-1".to_string(), Some(9.8))]
        );
        assert_eq!(sec.fixed[0].new_version.as_deref(), Some("3.0.14"));
        assert_eq!(sec.remaining.len(), 1);

        let report = HostReport {
            host: "fw13".into(),
            release: "25.11".into(),
            old_rev: "a".repeat(40),
            new_rev: "b".repeat(40),
            diff,
            security: Some(sec),
            direct: vec!["openssl".into(), "htop".into()],
        };
        let md = render_markdown(std::slice::from_ref(&report));
        assert!(md.contains("**1 CVE fixed** in 1 package."), "{md}");
        assert!(
            md.contains("| openssl | 3.0.13 | 3.0.14 | CVE-2024-1 (9.8, critical) |"),
            "{md}"
        );
        assert!(md.contains("### Patch updates (1)"), "{md}");
        assert!(
            md.contains("- `knixl install --host fw13 openssl@3.0.14`"),
            "{md}"
        );
        assert!(
            !md.contains("htop@"),
            "an unchanged direct package isn't offered: {md}"
        );
        let j = to_json(std::slice::from_ref(&report));
        assert_eq!(j["security_fixes"], 1);
        assert_eq!(j["hosts"][0]["changes"][0]["bump"], "patch");
    }

    #[test]
    fn no_reports_says_up_to_date() {
        assert!(render_markdown(&[]).contains("newest commit"));
        assert_eq!(to_json(&[])["updates"], false);
    }
}
