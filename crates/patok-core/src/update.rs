//! Release discovery and update decision logic for self-update.
//!
//! This module is pure: it contains no filesystem access, environment access or
//! printing. The side effects of `patok update` (downloading, verifying and
//! installing a release, and writing user-facing lines) live in the binary's
//! update module, which consumes [`UpdateOutcome`] and prints it.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::config::config_enum;

config_enum! {
    /// Which releases self-update considers.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
    #[schemars(rename_all = "lowercase")]
    pub enum UpdateChannel {
        #[default]
        Stable => "stable",
        Dev => "dev",
    }
}

/// GitHub API endpoint listing the 20 most recent releases.
pub const RELEASES_URL: &str = "https://api.github.com/repos/patok-dev/patok/releases?per_page=20";

/// Timeout applied to the release lookup.
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Minimal HTTP client abstraction so tests can substitute a fake.
pub trait HttpClient {
    /// GET `url` with the given timeout and return the response body.
    fn get(&self, url: &str, timeout: Duration) -> Result<String, HttpError>;

    /// GET `url` with the given timeout and write the response body to `dest`.
    fn download(&self, url: &str, dest: &Path, timeout: Duration) -> Result<(), HttpError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError(pub String);

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http request failed: {}", self.0)
    }
}

impl std::error::Error for HttpError {}

#[derive(Debug, PartialEq, Eq)]
pub enum DiscoveryError {
    Http(HttpError),
    Parse(String),
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(e) => e.fmt(f),
            Self::Parse(e) => write!(f, "invalid release listing: {e}"),
        }
    }
}

impl std::error::Error for DiscoveryError {}

#[derive(Debug, Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
}

/// A release that qualifies for self-update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    /// Download URLs of the `patok-*.tar.gz` archives.
    pub archives: Vec<Asset>,
    pub checksums_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

fn is_patok_archive(name: &str) -> bool {
    name.starts_with("patok") && name.ends_with(".tar.gz")
}

/// Query the most recent releases and return the first qualifying one, if any.
pub fn discover_release(
    client: &dyn HttpClient,
    channel: UpdateChannel,
) -> Result<Option<Release>, DiscoveryError> {
    let body = client
        .get(RELEASES_URL, LOOKUP_TIMEOUT)
        .map_err(DiscoveryError::Http)?;
    let releases: Vec<RawRelease> =
        serde_json::from_str(&body).map_err(|e| DiscoveryError::Parse(e.to_string()))?;
    Ok(select_release(releases, channel))
}

fn select_release(releases: Vec<RawRelease>, channel: UpdateChannel) -> Option<Release> {
    releases.into_iter().find_map(|r| {
        if r.draft || (r.prerelease && channel != UpdateChannel::Dev) {
            return None;
        }
        let checksums = r.assets.iter().find(|a| a.name == CHECKSUMS_ASSET)?;
        let archives: Vec<Asset> = r
            .assets
            .iter()
            .filter(|a| is_patok_archive(&a.name))
            .map(|a| Asset {
                name: a.name.clone(),
                url: a.browser_download_url.clone(),
            })
            .collect();
        if archives.is_empty() {
            return None;
        }
        Some(Release {
            tag: r.tag_name,
            archives,
            checksums_url: checksums.browser_download_url.clone(),
        })
    })
}

/// (os, arch, target triple) for every platform a release archive is
/// published for. This table is the single source of truth for the release
/// target set: [`platform_target`] and [`supported_targets`] read it, and the
/// release-contract integration test cross-checks `install.sh` and the
/// release workflow against it.
const PLATFORM_TARGETS: [(&str, &str, &str); 3] = [
    ("linux", "x86_64", "x86_64-unknown-linux-musl"),
    ("macos", "aarch64", "aarch64-apple-darwin"),
    ("macos", "x86_64", "x86_64-apple-darwin"),
];

/// The release target triple for a platform, or `None` when no release is published for it.
pub fn platform_target(os: &str, arch: &str) -> Option<&'static str> {
    PLATFORM_TARGETS
        .iter()
        .find_map(|(row_os, row_arch, target)| {
            (*row_os == os && *row_arch == arch).then_some(*target)
        })
}

/// Every target triple a release archive is published for, in table order.
pub fn supported_targets() -> impl Iterator<Item = &'static str> {
    PLATFORM_TARGETS.iter().map(|(_, _, target)| *target)
}

/// Name of the release archive for a target (`patok-<target>.tar.gz`).
pub fn archive_name(target: &str) -> String {
    format!("patok-{target}.tar.gz")
}

/// Name of the release asset holding the `<sha256>  <filename>` checksum lines.
pub const CHECKSUMS_ASSET: &str = "checksums";

/// The expected hash from the `<sha256>  <filename>` line whose filename equals `archive` exactly.
pub fn expected_checksum(checksums: &str, archive: &str) -> Option<String> {
    checksums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let (hash, name) = (parts.next()?, parts.next()?);
        (name == archive && parts.next().is_none()).then(|| hash.to_ascii_lowercase())
    })
}

/// Whether an executable path lies inside a Homebrew prefix.
pub fn is_homebrew_path(path: &Path) -> bool {
    let p = path.to_string_lossy();
    p.contains("/Cellar/") || p.contains("/Homebrew/") || p.contains("/linuxbrew/")
}

/// Result of `patok update`'s decision flow.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateOutcome {
    Homebrew,
    UnsupportedPlatform,
    /// The user should build from source, for the reason in the detail.
    SourceInstall(SourceInstallDetail),
    UpToDate,
    Available {
        release: Release,
        asset: Asset,
    },
}

/// Why self-update cannot proceed and the user should build from source.
#[derive(Debug, PartialEq, Eq)]
pub enum SourceInstallDetail {
    /// The release lookup failed.
    Lookup(DiscoveryError),
    /// No published release carries an archive for `target`.
    NoRelease { target: &'static str },
}

/// Run the update decision flow and return what `patok update` should do.
///
/// The caller prints the outcome; a Homebrew install short-circuits before any
/// release lookup.
pub fn check_update(
    client: &dyn HttpClient,
    channel: UpdateChannel,
    current: &str,
    exe: &Path,
    os: &str,
    arch: &str,
) -> UpdateOutcome {
    if is_homebrew_path(exe) {
        return UpdateOutcome::Homebrew;
    }
    let Some(target) = platform_target(os, arch) else {
        return UpdateOutcome::UnsupportedPlatform;
    };
    let wanted = archive_name(target);
    let release = match discover_release(client, channel) {
        Ok(Some(r)) if r.archives.iter().any(|a| a.name == wanted) => r,
        Ok(_) => {
            return UpdateOutcome::SourceInstall(SourceInstallDetail::NoRelease { target });
        }
        Err(e) => {
            return UpdateOutcome::SourceInstall(SourceInstallDetail::Lookup(e));
        }
    };
    let newer = match (
        release.tag.parse::<crate::version::Version>(),
        current.parse::<crate::version::Version>(),
    ) {
        (Ok(latest), Ok(cur)) => latest > cur,
        _ => false,
    };
    if !newer {
        return UpdateOutcome::UpToDate;
    }
    let asset = release
        .archives
        .iter()
        .find(|a| a.name == wanted)
        .cloned()
        .expect("checked above");
    UpdateOutcome::Available { release, asset }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::cell::RefCell;

    struct Fake {
        response: Result<String, HttpError>,
        calls: RefCell<Vec<(String, Duration)>>,
    }

    impl Fake {
        fn ok(v: Value) -> Self {
            Self {
                response: Ok(v.to_string()),
                calls: RefCell::default(),
            }
        }
    }

    impl HttpClient for Fake {
        fn get(&self, url: &str, timeout: Duration) -> Result<String, HttpError> {
            self.calls.borrow_mut().push((url.to_string(), timeout));
            self.response.clone()
        }

        fn download(&self, _url: &str, _dest: &Path, _timeout: Duration) -> Result<(), HttpError> {
            unreachable!("discovery tests never download")
        }
    }

    fn rel(tag: &str, draft: bool, pre: bool, assets: &[&str]) -> Value {
        json!({
            "tag_name": tag, "draft": draft, "prerelease": pre,
            "assets": assets.iter().map(|n| json!({
                "name": n, "browser_download_url": format!("https://x/{tag}/{n}")
            })).collect::<Vec<_>>(),
        })
    }

    const GOOD: [&str; 2] = ["patok-linux-x86_64.tar.gz", "checksums"];

    /// The one channel enum the config's `tui.update_channel` and the updater share:
    /// stable is the default and both spellings parse (trimmed, case-insensitive).
    #[test]
    fn the_update_channel_spells_stable_and_dev() {
        assert_eq!(UpdateChannel::default(), UpdateChannel::Stable);
        assert_eq!(
            UpdateChannel::parse("  STABLE "),
            Some(UpdateChannel::Stable)
        );
        assert_eq!(UpdateChannel::parse("dev"), Some(UpdateChannel::Dev));
        assert_eq!(UpdateChannel::Dev.as_str(), "dev");
        assert_eq!(UpdateChannel::Dev.to_string(), "dev");
    }

    #[test]
    fn uses_limit_and_timeout() {
        let fake = Fake::ok(json!([]));
        assert_eq!(
            discover_release(&fake, UpdateChannel::Stable).unwrap(),
            None
        );
        let calls = fake.calls.borrow();
        assert!(calls[0].0.contains("per_page=20"));
        assert_eq!(calls[0].1, Duration::from_secs(10));
    }

    #[test]
    fn skips_drafts_and_prereleases_on_stable() {
        let fake = Fake::ok(json!([
            rel("v3", true, false, &GOOD),
            rel("v2", false, true, &GOOD),
            rel("v1", false, false, &GOOD),
        ]));
        let r = discover_release(&fake, UpdateChannel::Stable)
            .unwrap()
            .unwrap();
        assert_eq!(r.tag, "v1");
    }

    #[test]
    fn dev_channel_accepts_prerelease_but_not_draft() {
        let fake = Fake::ok(json!([
            rel("v3", true, false, &GOOD),
            rel("v2", false, true, &GOOD),
            rel("v1", false, false, &GOOD),
        ]));
        let r = discover_release(&fake, UpdateChannel::Dev)
            .unwrap()
            .unwrap();
        assert_eq!(r.tag, "v2");
    }

    #[test]
    fn skips_releases_missing_checksums_or_archive() {
        let fake = Fake::ok(json!([
            rel("v3", false, false, &["patok-linux-x86_64.tar.gz"]),
            rel("v2", false, false, &["checksums", "other.zip"]),
            rel("v1", false, false, &GOOD),
        ]));
        let r = discover_release(&fake, UpdateChannel::Stable)
            .unwrap()
            .unwrap();
        assert_eq!(r.tag, "v1");
        assert_eq!(r.checksums_url, "https://x/v1/checksums");
        assert_eq!(r.archives[0].name, "patok-linux-x86_64.tar.gz");
    }

    #[test]
    fn none_when_nothing_qualifies() {
        let fake = Fake::ok(json!([rel("v1", true, false, &GOOD)]));
        assert_eq!(discover_release(&fake, UpdateChannel::Dev).unwrap(), None);
    }

    #[test]
    fn propagates_http_and_parse_errors() {
        let fake = Fake {
            response: Err(HttpError("timeout".into())),
            calls: RefCell::default(),
        };
        assert!(matches!(
            discover_release(&fake, UpdateChannel::Stable),
            Err(DiscoveryError::Http(_))
        ));
        let fake = Fake {
            response: Ok("nope".into()),
            calls: RefCell::default(),
        };
        assert!(matches!(
            discover_release(&fake, UpdateChannel::Stable),
            Err(DiscoveryError::Parse(_))
        ));
    }

    fn run(fake: &Fake, current: &str, exe: &str, os: &str, arch: &str) -> UpdateOutcome {
        check_update(
            fake,
            UpdateChannel::Stable,
            current,
            Path::new(exe),
            os,
            arch,
        )
    }

    const LINUX: [&str; 2] = ["patok-x86_64-unknown-linux-musl.tar.gz", "checksums"];

    #[test]
    fn update_homebrew_stops_before_lookup() {
        let fake = Fake::ok(json!([]));
        let o = run(
            &fake,
            "1.0.0",
            "/opt/homebrew/Cellar/patok/1.0.0/bin/patok",
            "macos",
            "aarch64",
        );
        assert_eq!(o, UpdateOutcome::Homebrew);
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn update_unsupported_platform() {
        let fake = Fake::ok(json!([]));
        let o = run(&fake, "1.0.0", "/usr/bin/patok", "linux", "aarch64");
        assert_eq!(o, UpdateOutcome::UnsupportedPlatform);
    }

    #[test]
    fn update_source_advice_on_none_or_failure() {
        let fake = Fake::ok(json!([]));
        assert_eq!(
            run(&fake, "1.0.0", "/x/patok", "linux", "x86_64"),
            UpdateOutcome::SourceInstall(SourceInstallDetail::NoRelease {
                target: "x86_64-unknown-linux-musl"
            })
        );
        let fake = Fake {
            response: Err(HttpError("down".into())),
            calls: RefCell::default(),
        };
        assert!(matches!(
            run(&fake, "1.0.0", "/x/patok", "linux", "x86_64"),
            UpdateOutcome::SourceInstall(SourceInstallDetail::Lookup(_))
        ));
    }

    #[test]
    fn update_up_to_date_and_available() {
        let fake = Fake::ok(json!([rel("v1.0.0", false, false, &LINUX)]));
        assert_eq!(
            run(&fake, "1.0.0", "/x/patok", "linux", "x86_64"),
            UpdateOutcome::UpToDate
        );
        let fake = Fake::ok(json!([rel("v1.1.0", false, false, &LINUX)]));
        assert!(matches!(
            run(&fake, "1.0.0", "/x/patok", "linux", "x86_64"),
            UpdateOutcome::Available { .. }
        ));
    }

    const ARCHIVE: &str = "patok-x86_64-unknown-linux-musl.tar.gz";
    // sha256("hello")
    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    #[test]
    fn checksum_requires_exact_filename() {
        let sums = format!("aaa  other-{ARCHIVE}\n{HELLO}  {ARCHIVE}\n");
        assert_eq!(expected_checksum(&sums, ARCHIVE).as_deref(), Some(HELLO));
        assert_eq!(
            expected_checksum("aaa  x-patok.tar.gz\n", "patok.tar.gz"),
            None
        );
    }
}
