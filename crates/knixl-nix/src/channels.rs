//! Official NixOS channel releases for a release branch, read from the releases.nixos.org
//! bucket: the commits Hydra tested and the binary cache built. `upgrade --refresh --keep`
//! chooses among these, never an arbitrary branch commit, so a constrained refresh still lands
//! on something the cache has.

/// One channel release, e.g. `nixos-25.11.12484.b6018f87da91`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRelease {
    pub name: String,
    /// The channel's commit count, which orders releases oldest to newest.
    pub revcount: u64,
    /// The abbreviated commit in the name (12 characters).
    pub short_rev: String,
}

fn release_prefix(release: &str) -> String {
    format!("nixos/{release}/")
}

/// Parse one page of the bucket's S3 listing: each release is a `<Key>` directly under the
/// release prefix. Returns the releases and the marker for the next page when truncated.
pub fn parse_listing(xml: &str, release: &str) -> (Vec<ChannelRelease>, Option<String>) {
    let prefix = release_prefix(release);
    let mut out = Vec::new();
    let mut last_key = None;
    for chunk in xml.split("<Key>").skip(1) {
        let Some(key) = chunk.split("</Key>").next() else {
            continue;
        };
        last_key = Some(key.to_string());
        let Some(name) = key.strip_prefix(&prefix) else {
            continue;
        };
        if name.contains('/') || !name.starts_with("nixos-") {
            continue;
        }
        let Some((head, short)) = name.rsplit_once('.') else {
            continue;
        };
        let digits: String = head
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        if let Ok(revcount) = digits.parse() {
            out.push(ChannelRelease {
                name: name.to_string(),
                revcount,
                short_rev: short.to_string(),
            });
        }
    }
    let truncated = xml.contains("<IsTruncated>true</IsTruncated>");
    (out, if truncated { last_key } else { None })
}

fn get(url: &str) -> Result<String, String> {
    match ureq::get(url).header("User-Agent", "knixl").call() {
        Ok(mut resp) => resp
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("reading {url}: {e}")),
        Err(ureq::Error::StatusCode(code)) => Err(format!("{url}: HTTP {code}")),
        Err(e) => Err(format!("{url}: {e}")),
    }
}

/// Every channel release for `release` (e.g. `25.11` or `unstable`), oldest first.
pub fn list(release: &str) -> Result<Vec<ChannelRelease>, String> {
    let prefix = release_prefix(release);
    let mut all = Vec::new();
    let mut marker = String::new();
    loop {
        let url = format!(
            "https://nix-releases.s3.amazonaws.com/?prefix={prefix}&delimiter=/&marker={marker}"
        );
        let (page, next) = parse_listing(&get(&url)?, release);
        all.extend(page);
        match next {
            Some(m) => marker = m,
            None => break,
        }
    }
    all.sort_by_key(|r| r.revcount);
    Ok(all)
}

/// The full commit of a channel release.
pub fn full_rev(release: &str, name: &str) -> Result<String, String> {
    let rev = get(&format!(
        "https://releases.nixos.org/nixos/{release}/{name}/git-revision"
    ))?;
    let rev = rev.trim().to_string();
    if rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(rev)
    } else {
        Err(format!("{name}: unexpected git-revision {rev:?}"))
    }
}

/// `*`-wildcard match, for version constraints like `8.19.*` or `6.*`.
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let mut rest = text;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(at) => rest = &rest[at + part.len()..],
                None => return false,
            }
        }
    }
    true
}

/// The newest index in `0..n` for which `ok` holds, given that it holds for a prefix of the
/// range and then stops (a kept package leaves its version range and doesn't come back), in
/// about log2(n) calls. `None` when it holds for none.
pub fn newest_passing(
    n: usize,
    mut ok: impl FnMut(usize) -> Result<bool, String>,
) -> Result<Option<usize>, String> {
    if n == 0 {
        return Ok(None);
    }
    if ok(n - 1)? {
        return Ok(Some(n - 1));
    }
    let (mut lo, mut hi) = (0usize, n - 1); // hi is known to fail
    if !ok(0)? {
        return Ok(None);
    }
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if ok(mid)? {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Ok(Some(lo))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "<ListBucketResult><IsTruncated>false</IsTruncated><Contents><Key>nixos/25.11/nixos-25.11.12484.b6018f87da91</Key></Contents><Contents><Key>nixos/25.11/nixos-25.11.10031.755f5aa91337</Key></Contents><Contents><Key>nixos/25.11/nixos-25.11.10031.755f5aa91337/git-revision</Key></Contents></ListBucketResult>";

    #[test]
    fn parses_releases_from_the_listing() {
        let (rel, next) = parse_listing(PAGE, "25.11");
        assert_eq!(next, None);
        assert_eq!(rel.len(), 2);
        assert_eq!(rel[0].revcount, 12484);
        assert_eq!(rel[0].short_rev, "b6018f87da91");
        let (unstable, _) = parse_listing(
            "<Key>nixos/unstable/nixos-26.05pre912345.abcdef123456</Key>",
            "unstable",
        );
        assert_eq!(unstable[0].revcount, 912345);
    }

    #[test]
    fn globs_match_version_ranges() {
        assert!(glob_matches("8.19.*", "8.19.0"));
        assert!(!glob_matches("8.19.*", "8.20.0"));
        assert!(glob_matches("6.*", "6.12.93"));
        assert!(glob_matches("3.0.13", "3.0.13"));
        assert!(!glob_matches("3.0", "3.0.13"));
    }

    #[test]
    fn finds_the_newest_passing_index() {
        let pass_upto = |k: usize| move |i: usize| Ok(i <= k);
        assert_eq!(newest_passing(10, pass_upto(9)).unwrap(), Some(9));
        assert_eq!(newest_passing(10, pass_upto(4)).unwrap(), Some(4));
        assert_eq!(newest_passing(10, pass_upto(0)).unwrap(), Some(0));
        assert_eq!(newest_passing(10, |_| Ok(false)).unwrap(), None);
        assert_eq!(newest_passing(0, |_| Ok(true)).unwrap(), None);
    }
}
