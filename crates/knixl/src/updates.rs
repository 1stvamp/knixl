//! `knixl updates` and the selective refresh: what `upgrade --refresh` would change, and
//! `--keep`, which picks the newest channel release that keeps packages in range.

use crate::Code;
use crate::Ctx;
use std::collections::BTreeMap;

/// The newest official channel release of `release` after `current` whose versions of every
/// kept package match its pattern (`upgrade --refresh --keep`), or `current` when none does.
/// Releases are searched by bisection, evaluating each candidate's package versions.
pub(crate) fn constrained_refresh(
    release: &str,
    system: &str,
    current: &str,
    keeps: &[(String, String)],
) -> Result<String, String> {
    use knixl_nix::channels;
    let all = channels::list(release)?;
    let start = all
        .iter()
        .position(|r| current.starts_with(&r.short_rev))
        .ok_or_else(|| {
            format!("nixpkgs {current} isn't a {release} channel release, so --keep can't tell which releases come after it")
        })?;
    let candidates = &all[start + 1..];
    eprintln!(
        "checking {} newer {release} channel releases against --keep",
        candidates.len()
    );
    let mut revs: BTreeMap<usize, String> = BTreeMap::new();
    let found = channels::newest_passing(candidates.len(), |i| {
        let rev = channels::full_rev(release, &candidates[i].name)?;
        revs.insert(i, rev.clone());
        for (pkg, pattern) in keeps {
            let attr = format!("github:NixOS/nixpkgs/{rev}#legacyPackages.{system}.{pkg}.version");
            let version = run_capture(
                std::path::Path::new("."),
                std::path::Path::new("nix"),
                &[
                    "--extra-experimental-features",
                    "nix-command flakes",
                    "eval",
                    "--raw",
                    &attr,
                ],
            )?;
            if !channels::glob_matches(pattern, &version) {
                eprintln!(
                    "  {}: {pkg} {version} is outside {pattern}",
                    candidates[i].name
                );
                return Ok(false);
            }
        }
        eprintln!("  {}: every kept package matches", candidates[i].name);
        Ok(true)
    })?;
    Ok(match found {
        Some(i) => revs
            .remove(&i)
            .expect("a passing release had its rev looked up"),
        None => current.to_string(),
    })
}

/// Run `prog args` in `cwd`, returning trimmed stdout, or the stderr tail on failure.
fn run_capture(
    cwd: &std::path::Path,
    prog: &std::path::Path,
    args: &[&str],
) -> Result<String, String> {
    let out = std::process::Command::new(prog)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("{}: {e}", prog.display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(6).collect();
        Err(format!(
            "{} {}: {}",
            prog.display(),
            args.join(" "),
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        ))
    }
}

/// Copy a project for `knixl updates` to work on, leaving out `.git` and symlinks (`result`
/// links and the like), so the original is never touched.
fn copy_project(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let ty = entry.file_type()?;
        if name == ".git" || ty.is_symlink() {
            continue;
        }
        let dest = to.join(&name);
        if ty.is_dir() {
            copy_project(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

/// The nixpkgs baselines recorded in a project's lock, by host: (release, rev).
fn lock_baselines(root: &std::path::Path) -> BTreeMap<String, (String, String)> {
    std::fs::read_to_string(root.join("knixl.lock.kdl"))
        .ok()
        .and_then(|t| knixl_lock::Lock::parse(&t).ok())
        .map(|l| {
            l.baselines
                .into_iter()
                .map(|(h, b)| (h, (b.release, b.nixpkgs_rev)))
                .collect()
        })
        .unwrap_or_default()
}

/// `knixl updates`: refresh one copy of the project, evaluate each host that moved in that
/// copy and in an untouched one, and report the difference (#142). Nothing in the project
/// changes; the copies live in the temp directory and are removed afterwards.
pub(crate) fn cmd_updates(ctx: &Ctx, json: bool) -> Code {
    if !ctx.root.join("generated/flake.nix").is_file() {
        eprintln!(
            "knixl: {}",
            updates_report(&ctx.root).err().unwrap_or_default()
        );
        return Code::Validation;
    }
    match updates_report(&ctx.root) {
        Ok(reports) => {
            if json {
                println!("{}", knixl_pipeline::updates::to_json(&reports));
            } else {
                print!("{}", knixl_pipeline::updates::render_markdown(&reports));
            }
            Code::Clean
        }
        Err(e) => {
            eprintln!("knixl: {e}");
            Code::Internal
        }
    }
}

/// Carry out the Updates screen's choice by running knixl's own commands, so the work and
/// its output are exactly what the CLI does: the refresh (then generate), or each pin.
pub(crate) fn commit_tui_updates(
    refresh: bool,
    if_security: bool,
    pins: &[(String, String, String)],
) -> Code {
    let Ok(me) = std::env::current_exe() else {
        return Code::Internal;
    };
    let run = |args: &[&str]| -> Code {
        println!("$ knixl {}", args.join(" "));
        match std::process::Command::new(&me).args(args).status() {
            Ok(s) if s.success() => Code::Clean,
            Ok(_) => Code::Validation,
            Err(e) => {
                eprintln!("knixl: {e}");
                Code::Internal
            }
        }
    };
    if refresh {
        let mut args = vec!["upgrade", "--refresh", "--yes"];
        if if_security {
            args.push("--if-security");
        }
        let code = run(&args);
        if code != Code::Clean {
            return code;
        }
        return run(&["generate"]);
    }
    for (host, attr, version) in pins {
        let spec = format!("{attr}@{version}");
        let code = run(&["install", "--host", host, &spec, "--yes"]);
        if code != Code::Clean {
            return code;
        }
    }
    Code::Clean
}

/// A host's own unpinned `package` nodes (no `version=`, no `from=`), by attribute name.
fn direct_packages(root: &std::path::Path, host: &str) -> Vec<String> {
    let hosts = knixl_pipeline::install::list_hosts(root).unwrap_or_default();
    let Some(h) = hosts.iter().find(|h| h.name == host) else {
        return Vec::new();
    };
    let Ok(src) = std::fs::read_to_string(&h.path) else {
        return Vec::new();
    };
    let Ok(doc) = knixl_kdl::parse(&src) else {
        return Vec::new();
    };
    doc.nodes()
        .iter()
        .find(|n| n.name().value() == "host")
        .and_then(|n| n.children())
        .map(|c| {
            c.nodes()
                .iter()
                .filter(|n| n.name().value() == "package")
                .filter(|n| n.get("version").is_none() && n.get("from").is_none())
                .filter_map(knixl_kdl::first_arg_str)
                .collect()
        })
        .unwrap_or_default()
}

/// Run the `knixl updates` analysis in a private scratch directory, removed afterwards.
pub(crate) fn updates_report(
    root: &std::path::Path,
) -> Result<Vec<knixl_pipeline::updates::HostReport>, String> {
    if !root.join("generated/flake.nix").is_file() {
        return Err("updates evaluates each host through the generated flake: add a system block to knixl.kdl and run knixl generate".into());
    }
    let tmp = std::env::temp_dir().join(format!("knixl-updates-{}", std::process::id()));
    // Owner-only, and never an existing directory: another user could have made the
    // predictable path first to read or tamper with the copied project.
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&tmp)
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
    }
    let result = updates_in(root, &tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn updates_in(
    root: &std::path::Path,
    tmp: &std::path::Path,
) -> Result<Vec<knixl_pipeline::updates::HostReport>, String> {
    use knixl_pipeline::updates;
    use std::path::Path;
    let (old_dir, new_dir) = (tmp.join("old"), tmp.join("new"));
    for d in [&old_dir, &new_dir] {
        copy_project(root, d).map_err(|e| format!("copying the project: {e}"))?;
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let (git, nix) = (Path::new("git"), Path::new("nix"));
    let flakes = ["--extra-experimental-features", "nix-command flakes"];
    eprintln!("refreshing nixpkgs in a copy of the project");
    run_capture(&new_dir, &me, &["upgrade", "--refresh", "--yes"])?;
    run_capture(&new_dir, &me, &["generate"])?;

    let before = lock_baselines(&old_dir);
    let after = lock_baselines(&new_dir);
    let moved: Vec<(String, String, String, String)> = after
        .iter()
        .filter_map(|(host, (release, rev))| {
            let (_, old) = before.get(host)?;
            (old != rev).then(|| (host.clone(), release.clone(), old.clone(), rev.clone()))
        })
        .collect();
    if moved.is_empty() {
        return Ok(Vec::new());
    }

    for d in [&old_dir, &new_dir] {
        run_capture(d, git, &["init", "-q"])?;
        run_capture(d, git, &["add", "-A"])?;
    }
    if new_dir.join("generated/flake.lock").is_file() {
        let mut args = flakes.to_vec();
        args.extend(["flake", "lock"]);
        run_capture(&new_dir.join("generated"), nix, &args)?;
        run_capture(&new_dir, git, &["add", "-A"])?;
    }

    let vulnix_bin = std::env::var("KNIXL_VULNIX").unwrap_or_else(|_| "vulnix".into());
    let vulnix = Path::new(&vulnix_bin);
    let mut vulnix_missing = false;
    let mut scan = |drv: &str| -> Option<Vec<updates::Vuln>> {
        if vulnix_missing {
            return None;
        }
        let out = std::process::Command::new(vulnix)
            .args(["--json", drv])
            .output();
        match out {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "warning: {} not found, so security fixes are not checked (set KNIXL_VULNIX)",
                    vulnix.display()
                );
                vulnix_missing = true;
                None
            }
            Err(e) => {
                eprintln!("warning: vulnix: {e}");
                None
            }
            // vulnix exits non-zero when it finds anything, so judge it by its output.
            Ok(o) => match updates::parse_vulnix(&String::from_utf8_lossy(&o.stdout)) {
                Ok(v) => Some(v),
                Err(e) => {
                    eprintln!("warning: {e}");
                    None
                }
            },
        }
    };

    let mut reports = Vec::new();
    for (host, release, old_rev, new_rev) in moved {
        let attr = format!(
            "./generated#nixosConfigurations.\"{host}\".config.system.build.toplevel.drvPath"
        );
        let mut drvs = Vec::new();
        for d in [&old_dir, &new_dir] {
            eprintln!(
                "evaluating {host} in {}",
                d.file_name().unwrap().to_string_lossy()
            );
            let mut args = flakes.to_vec();
            args.extend(["eval", "--raw", attr.as_str()]);
            drvs.push(run_capture(d, nix, &args)?);
        }
        let closure = |drv: &str| -> Result<Vec<String>, String> {
            run_capture(Path::new("."), Path::new("nix-store"), &["-qR", drv])
                .map(|s| s.lines().map(str::to_string).collect())
        };
        let diff = updates::diff_closures(&closure(&drvs[0])?, &closure(&drvs[1])?);
        eprintln!("scanning {host} with vulnix");
        let security = match (scan(&drvs[0]), scan(&drvs[1])) {
            (Some(old), Some(new)) => Some(updates::security(&old, &new, &diff)),
            _ => None,
        };
        let direct = direct_packages(root, &host);
        reports.push(updates::HostReport {
            host,
            release,
            old_rev,
            new_rev,
            diff,
            security,
            direct,
        });
    }
    Ok(reports)
}
