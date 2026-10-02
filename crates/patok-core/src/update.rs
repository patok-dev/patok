//! Release discovery for self-update.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// GitHub API endpoint listing the 20 most recent releases.
pub const RELEASES_URL: &str = "https://api.github.com/repos/patok-dev/patok/releases?per_page=20";

/// Timeout applied to the release lookup.
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout applied to each download (archive and checksums).
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

const CHECKSUMS_ASSET: &str = "checksums";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Channel {
    #[default]
    Stable,
    Dev,
}

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

#[derive(Debug)]
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
    channel: Channel,
) -> Result<Option<Release>, DiscoveryError> {
    let body = client
        .get(RELEASES_URL, LOOKUP_TIMEOUT)
        .map_err(DiscoveryError::Http)?;
    let releases: Vec<RawRelease> =
        serde_json::from_str(&body).map_err(|e| DiscoveryError::Parse(e.to_string()))?;
    Ok(select_release(releases, channel))
}

fn select_release(releases: Vec<RawRelease>, channel: Channel) -> Option<Release> {
    releases.into_iter().find_map(|r| {
        if r.draft || (r.prerelease && channel != Channel::Dev) {
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

/// Build-from-source advice shown when self-update cannot proceed.
pub const SOURCE_INSTALL_ADVICE: &str =
    "Build from source: cargo install --git https://github.com/patok-dev/patok patok";

/// Instruction shown for Homebrew installs, which `patok update` never touches.
pub const BREW_UPGRADE_ADVICE: &str = "Installed via Homebrew; run: brew upgrade patok";

/// The release target triple for a platform, or `None` when no release is published for it.
pub fn platform_target(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        _ => None,
    }
}

/// Name of the release archive for a target (`patok-<target>.tar.gz`).
pub fn archive_name(target: &str) -> String {
    format!("patok-{target}.tar.gz")
}

#[derive(Debug)]
pub enum DownloadError {
    Io(std::io::Error),
    Http(HttpError),
    /// The checksums file has no line for the archive.
    MissingChecksum(String),
    Mismatch {
        archive: String,
        expected: String,
        actual: String,
    },
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error during update: {e}"),
            Self::Http(e) => e.fmt(f),
            Self::MissingChecksum(a) => write!(f, "no checksum listed for {a}"),
            Self::Mismatch {
                archive,
                expected,
                actual,
            } => write!(
                f,
                "checksum mismatch for {archive} (expected {expected}, got {actual})"
            ),
        }
    }
}

impl std::error::Error for DownloadError {}

impl From<std::io::Error> for DownloadError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<HttpError> for DownloadError {
    fn from(e: HttpError) -> Self {
        Self::Http(e)
    }
}

/// Per-version temp directory used to stage an update.
pub fn update_temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("patok-update-{tag}"))
}

/// The expected hash from the `<sha256>  <filename>` line whose filename equals `archive` exactly.
pub fn expected_checksum(checksums: &str, archive: &str) -> Option<String> {
    checksums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let (hash, name) = (parts.next()?, parts.next()?);
        (name == archive && parts.next().is_none()).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Download the archive and checksums into `dir` and verify the archive's SHA-256.
///
/// Returns the archive path. On any failure `dir` is removed and nothing else is touched.
pub fn download_verified(
    client: &dyn HttpClient,
    release: &Release,
    asset: &Asset,
    dir: &Path,
) -> Result<PathBuf, DownloadError> {
    let result = (|| {
        std::fs::create_dir_all(dir)?;
        let archive_path = dir.join(&asset.name);
        let checksums_path = dir.join(CHECKSUMS_ASSET);
        client.download(&asset.url, &archive_path, DOWNLOAD_TIMEOUT)?;
        client.download(&release.checksums_url, &checksums_path, DOWNLOAD_TIMEOUT)?;
        let checksums = std::fs::read_to_string(&checksums_path)?;
        let expected = expected_checksum(&checksums, &asset.name)
            .ok_or_else(|| DownloadError::MissingChecksum(asset.name.clone()))?;
        let actual = sha256_file(&archive_path)?;
        if actual != expected {
            return Err(DownloadError::Mismatch {
                archive: asset.name.clone(),
                expected,
                actual,
            });
        }
        Ok(archive_path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(dir);
    }
    result
}

/// Replace `exe` with `new_binary`: rename the current file to a backup, copy the new one in,
/// set mode 755 and delete the backup. If the copy or chmod fails, the backup is restored.
pub fn replace_executable(exe: &Path, new_binary: &Path) -> std::io::Result<()> {
    let mut backup_name = exe.as_os_str().to_owned();
    backup_name.push(".bak");
    let backup = PathBuf::from(backup_name);
    std::fs::rename(exe, &backup)?;
    let installed = std::fs::copy(new_binary, exe).and_then(|_| set_executable(exe));
    match installed {
        Ok(()) => {
            let _ = std::fs::remove_file(&backup);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(exe);
            std::fs::rename(&backup, exe)?;
            Err(e)
        }
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Whether an executable path lies inside a Homebrew prefix.
pub fn is_homebrew_path(path: &std::path::Path) -> bool {
    let p = path.to_string_lossy();
    p.contains("/Cellar/") || p.contains("/Homebrew/") || p.contains("/linuxbrew/")
}

/// Result of `patok update`'s decision flow.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateOutcome {
    Homebrew,
    UnsupportedPlatform,
    SourceInstall,
    UpToDate,
    Available { release: Release, asset: Asset },
}

/// Run the update decision flow, writing user-facing lines to `out`.
pub fn check_update(
    out: &mut dyn std::io::Write,
    client: &dyn HttpClient,
    channel: Channel,
    current: &str,
    exe: &std::path::Path,
    os: &str,
    arch: &str,
) -> std::io::Result<UpdateOutcome> {
    writeln!(out, "patok {current}")?;
    if is_homebrew_path(exe) {
        writeln!(out, "{BREW_UPGRADE_ADVICE}")?;
        return Ok(UpdateOutcome::Homebrew);
    }
    let Some(target) = platform_target(os, arch) else {
        writeln!(out, "No release is published for {os}/{arch}.")?;
        writeln!(out, "{SOURCE_INSTALL_ADVICE}")?;
        return Ok(UpdateOutcome::UnsupportedPlatform);
    };
    let wanted = archive_name(target);
    let release = match discover_release(client, channel) {
        Ok(Some(r)) if r.archives.iter().any(|a| a.name == wanted) => r,
        Ok(_) => {
            writeln!(out, "No qualifying release found for {target}.")?;
            writeln!(out, "{SOURCE_INSTALL_ADVICE}")?;
            return Ok(UpdateOutcome::SourceInstall);
        }
        Err(e) => {
            writeln!(out, "Could not look up releases: {e}")?;
            writeln!(out, "{SOURCE_INSTALL_ADVICE}")?;
            return Ok(UpdateOutcome::SourceInstall);
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
        writeln!(out, "patok is up to date.")?;
        return Ok(UpdateOutcome::UpToDate);
    }
    let asset = release
        .archives
        .iter()
        .find(|a| a.name == wanted)
        .cloned()
        .expect("checked above");
    writeln!(out, "New release available: {}", release.tag)?;
    Ok(UpdateOutcome::Available { release, asset })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::cell::RefCell;

    struct Fake {
        response: Result<String, HttpError>,
        calls: RefCell<Vec<(String, Duration)>>,
        files: std::collections::HashMap<String, Vec<u8>>,
    }

    impl Fake {
        fn ok(v: Value) -> Self {
            Self {
                response: Ok(v.to_string()),
                calls: RefCell::default(),
                files: Default::default(),
            }
        }
    }

    impl HttpClient for Fake {
        fn get(&self, url: &str, timeout: Duration) -> Result<String, HttpError> {
            self.calls.borrow_mut().push((url.to_string(), timeout));
            self.response.clone()
        }

        fn download(&self, url: &str, dest: &Path, timeout: Duration) -> Result<(), HttpError> {
            self.calls.borrow_mut().push((url.to_string(), timeout));
            let name = url.rsplit('/').next().unwrap();
            let body = self.files.get(name).ok_or(HttpError("404".into()))?;
            std::fs::write(dest, body).map_err(|e| HttpError(e.to_string()))
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

    #[test]
    fn uses_limit_and_timeout() {
        let fake = Fake::ok(json!([]));
        assert_eq!(discover_release(&fake, Channel::Stable).unwrap(), None);
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
        let r = discover_release(&fake, Channel::Stable).unwrap().unwrap();
        assert_eq!(r.tag, "v1");
    }

    #[test]
    fn dev_channel_accepts_prerelease_but_not_draft() {
        let fake = Fake::ok(json!([
            rel("v3", true, false, &GOOD),
            rel("v2", false, true, &GOOD),
            rel("v1", false, false, &GOOD),
        ]));
        let r = discover_release(&fake, Channel::Dev).unwrap().unwrap();
        assert_eq!(r.tag, "v2");
    }

    #[test]
    fn skips_releases_missing_checksums_or_archive() {
        let fake = Fake::ok(json!([
            rel("v3", false, false, &["patok-linux-x86_64.tar.gz"]),
            rel("v2", false, false, &["checksums", "other.zip"]),
            rel("v1", false, false, &GOOD),
        ]));
        let r = discover_release(&fake, Channel::Stable).unwrap().unwrap();
        assert_eq!(r.tag, "v1");
        assert_eq!(r.checksums_url, "https://x/v1/checksums");
        assert_eq!(r.archives[0].name, "patok-linux-x86_64.tar.gz");
    }

    #[test]
    fn none_when_nothing_qualifies() {
        let fake = Fake::ok(json!([rel("v1", true, false, &GOOD)]));
        assert_eq!(discover_release(&fake, Channel::Dev).unwrap(), None);
    }

    #[test]
    fn propagates_http_and_parse_errors() {
        let fake = Fake {
            response: Err(HttpError("timeout".into())),
            calls: RefCell::default(),
            files: Default::default(),
        };
        assert!(matches!(
            discover_release(&fake, Channel::Stable),
            Err(DiscoveryError::Http(_))
        ));
        let fake = Fake {
            response: Ok("nope".into()),
            calls: RefCell::default(),
            files: Default::default(),
        };
        assert!(matches!(
            discover_release(&fake, Channel::Stable),
            Err(DiscoveryError::Parse(_))
        ));
    }

    fn run(fake: &Fake, current: &str, exe: &str, os: &str, arch: &str) -> (UpdateOutcome, String) {
        let mut out = Vec::new();
        let o = check_update(
            &mut out,
            fake,
            Channel::Stable,
            current,
            std::path::Path::new(exe),
            os,
            arch,
        )
        .unwrap();
        (o, String::from_utf8(out).unwrap())
    }

    const LINUX: [&str; 2] = ["patok-x86_64-unknown-linux-musl.tar.gz", "checksums"];

    #[test]
    fn update_homebrew_stops_before_lookup() {
        let fake = Fake::ok(json!([]));
        let (o, out) = run(
            &fake,
            "1.0.0",
            "/opt/homebrew/Cellar/patok/1.0.0/bin/patok",
            "macos",
            "aarch64",
        );
        assert_eq!(o, UpdateOutcome::Homebrew);
        assert!(out.starts_with("patok 1.0.0\n") && out.contains("brew upgrade"));
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn update_unsupported_platform() {
        let fake = Fake::ok(json!([]));
        let (o, out) = run(&fake, "1.0.0", "/usr/bin/patok", "linux", "aarch64");
        assert_eq!(o, UpdateOutcome::UnsupportedPlatform);
        assert!(out.contains("Build from source"));
    }

    #[test]
    fn update_source_advice_on_none_or_failure() {
        let fake = Fake::ok(json!([]));
        assert_eq!(
            run(&fake, "1.0.0", "/x/patok", "linux", "x86_64").0,
            UpdateOutcome::SourceInstall
        );
        let fake = Fake {
            response: Err(HttpError("down".into())),
            calls: RefCell::default(),
            files: Default::default(),
        };
        let (o, out) = run(&fake, "1.0.0", "/x/patok", "linux", "x86_64");
        assert_eq!(o, UpdateOutcome::SourceInstall);
        assert!(out.contains("Build from source"));
    }

    #[test]
    fn update_up_to_date_and_available() {
        let fake = Fake::ok(json!([rel("v1.0.0", false, false, &LINUX)]));
        let (o, out) = run(&fake, "1.0.0", "/x/patok", "linux", "x86_64");
        assert_eq!(o, UpdateOutcome::UpToDate);
        assert!(out.contains("up to date"));
        let fake = Fake::ok(json!([rel("v1.1.0", false, false, &LINUX)]));
        let (o, _) = run(&fake, "1.0.0", "/x/patok", "linux", "x86_64");
        assert!(matches!(o, UpdateOutcome::Available { .. }));
    }

    const ARCHIVE: &str = "patok-x86_64-unknown-linux-musl.tar.gz";
    // sha256("hello")
    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn download_fixture(checksums: &str, body: &[u8]) -> (Fake, Release, Asset) {
        let mut fake = Fake::ok(json!([]));
        fake.files.insert(ARCHIVE.into(), body.to_vec());
        fake.files.insert("checksums".into(), checksums.into());
        let release = Release {
            tag: "v1".into(),
            archives: vec![],
            checksums_url: "https://x/v1/checksums".into(),
        };
        let asset = Asset {
            name: ARCHIVE.into(),
            url: format!("https://x/v1/{ARCHIVE}"),
        };
        (fake, release, asset)
    }

    #[test]
    fn checksum_requires_exact_filename() {
        let sums = format!("aaa  other-{ARCHIVE}\n{HELLO}  {ARCHIVE}\n");
        assert_eq!(expected_checksum(&sums, ARCHIVE).as_deref(), Some(HELLO));
        assert_eq!(
            expected_checksum("aaa  x-patok.tar.gz\n", "patok.tar.gz"),
            None
        );
    }

    #[test]
    fn replace_executable_swaps_and_removes_backup() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("patok");
        let new = dir.path().join("new");
        std::fs::write(&exe, "old").unwrap();
        std::fs::write(&new, "new").unwrap();
        replace_executable(&exe, &new).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert!(!dir.path().join("patok.bak").exists());
    }

    #[test]
    fn replace_executable_restores_backup_on_copy_failure() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("patok");
        std::fs::write(&exe, "old").unwrap();
        let missing = dir.path().join("missing");
        assert!(replace_executable(&exe, &missing).is_err());
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old");
        assert!(!dir.path().join("patok.bak").exists());
    }

    #[test]
    fn download_verifies_and_uses_cap() {
        let (fake, rel, asset) = download_fixture(&format!("{HELLO}  {ARCHIVE}\n"), b"hello");
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("v1");
        let p = download_verified(&fake, &rel, &asset, &d).unwrap();
        assert_eq!(std::fs::read(p).unwrap(), b"hello");
        assert!(fake.calls.borrow().iter().all(|c| c.1 == DOWNLOAD_TIMEOUT));
    }

    #[test]
    fn download_aborts_on_mismatch_and_missing_line() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("v1");
        let (fake, rel, asset) = download_fixture(&format!("{HELLO}  {ARCHIVE}\n"), b"tampered");
        let err = download_verified(&fake, &rel, &asset, &d).unwrap_err();
        assert!(matches!(err, DownloadError::Mismatch { .. }));
        assert!(!d.exists());
        let (fake, rel, asset) = download_fixture(&format!("{HELLO}  x{ARCHIVE}\n"), b"hello");
        let err = download_verified(&fake, &rel, &asset, &d).unwrap_err();
        assert!(matches!(err, DownloadError::MissingChecksum(_)));
        assert!(!d.exists());
    }
}
