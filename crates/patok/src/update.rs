//! `patok update`: check for a newer release and install it.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, bail};
use patok_core::config::{ConfigFiles, load_tui};
use patok_core::update::{
    Asset, HttpClient, HttpError, Release, SourceInstallDetail, UpdateChannel, UpdateOutcome,
    check_update, expected_checksum,
};
use sha2::{Digest, Sha256};

/// Build-from-source advice shown when self-update cannot proceed.
const SOURCE_INSTALL_ADVICE: &str =
    "Build from source: cargo install --git https://github.com/patok-dev/patok patok";

/// Instruction shown for Homebrew installs, which `patok update` never touches.
const BREW_UPGRADE_ADVICE: &str = "Installed via Homebrew; run: brew upgrade patok";

const CHECKSUMS_ASSET: &str = "checksums";

/// Timeout applied to each download (archive and checksums).
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// HTTP client that shells out to `curl`.
struct CurlClient;

impl CurlClient {
    /// Run the shared `curl` invocation: the common flags, the timeout, the
    /// headers, an optional `--output dest` and `url`. Returns the output when
    /// curl exits successfully.
    fn curl(
        url: &str,
        timeout: Duration,
        dest: Option<&Path>,
        accept_json: bool,
    ) -> Result<std::process::Output, HttpError> {
        let mut command = Command::new("curl");
        command
            .args(["--silent", "--show-error", "--fail", "--location"])
            .args(["--max-time", &timeout.as_secs().to_string()]);
        if accept_json {
            command.args(["--header", "Accept: application/vnd.github+json"]);
        }
        command.args(["--header", "User-Agent: patok"]);
        if let Some(dest) = dest {
            command.arg("--output").arg(dest);
        }
        let output = command
            .arg(url)
            .output()
            .map_err(|e| HttpError(format!("cannot run curl: {e}")))?;
        if !output.status.success() {
            return Err(HttpError(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(output)
    }
}

impl HttpClient for CurlClient {
    fn get(&self, url: &str, timeout: Duration) -> Result<String, HttpError> {
        let output = Self::curl(url, timeout, None, true)?;
        String::from_utf8(output.stdout).map_err(|e| HttpError(e.to_string()))
    }

    fn download(&self, url: &str, dest: &Path, timeout: Duration) -> Result<(), HttpError> {
        Self::curl(url, timeout, Some(dest), false).map(|_| ())
    }
}

#[derive(Debug)]
enum DownloadError {
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
fn update_temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("patok-update-{tag}"))
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
fn download_verified(
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
fn replace_executable(exe: &Path, new_binary: &Path) -> std::io::Result<()> {
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

/// The `--channel` values of `patok update`.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum ChannelArg {
    Stable,
    Dev,
}

impl From<ChannelArg> for UpdateChannel {
    fn from(arg: ChannelArg) -> Self {
        match arg {
            ChannelArg::Stable => UpdateChannel::Stable,
            ChannelArg::Dev => UpdateChannel::Dev,
        }
    }
}

/// The channel `patok update` uses: an explicit `--channel` flag wins over the
/// `tui.update_channel` setting, which in turn wins over the stable default when
/// neither is present.
fn resolve_channel(
    flag: Option<ChannelArg>,
    files: &ConfigFiles,
    project_dir: &Path,
    data_dir: Option<&Path>,
) -> (UpdateChannel, Vec<String>) {
    let flag = flag.map(UpdateChannel::from);
    let (settings, warnings) = load_tui(files, project_dir, data_dir);
    (flag.unwrap_or(settings.update_channel), warnings)
}

pub fn run(flag: Option<ChannelArg>, project: &Path) -> anyhow::Result<()> {
    let files = ConfigFiles::from_env();
    let data_dir = patok_core::paths::project_data_dir_from_env(project);
    let (channel, warnings) = resolve_channel(flag, &files, project, data_dir.as_deref());
    for warning in warnings {
        eprintln!("patok: {warning}");
    }
    let exe = std::env::current_exe().unwrap_or_default();
    run_with(
        &mut std::io::stdout(),
        &CurlClient,
        channel,
        env!("CARGO_PKG_VERSION"),
        &exe,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

/// The whole `patok update` flow against an arbitrary release source.
fn run_with(
    out: &mut dyn std::io::Write,
    client: &dyn HttpClient,
    channel: UpdateChannel,
    current: &str,
    exe: &Path,
    os: &str,
    arch: &str,
) -> anyhow::Result<()> {
    let outcome = check_update(client, channel, current, exe, os, arch);
    report(out, &outcome, current, os, arch)?;
    if let UpdateOutcome::Available { release, asset } = outcome {
        let dir = update_temp_dir(&release.tag);
        let result = install(client, &dir, &release, &asset, exe);
        let _ = std::fs::remove_dir_all(&dir);
        result?;
        writeln!(out, "patok updated to {}", release.tag)?;
    }
    Ok(())
}

/// Write the user-facing lines for `outcome` in the order `patok update` prints them.
fn report(
    out: &mut dyn std::io::Write,
    outcome: &UpdateOutcome,
    current: &str,
    os: &str,
    arch: &str,
) -> std::io::Result<()> {
    writeln!(out, "patok {current}")?;
    match outcome {
        UpdateOutcome::Homebrew => writeln!(out, "{BREW_UPGRADE_ADVICE}")?,
        UpdateOutcome::UnsupportedPlatform => {
            writeln!(out, "No release is published for {os}/{arch}.")?;
            writeln!(out, "{SOURCE_INSTALL_ADVICE}")?;
        }
        UpdateOutcome::SourceInstall(SourceInstallDetail::Lookup(e)) => {
            writeln!(out, "Could not look up releases: {e}")?;
            writeln!(out, "{SOURCE_INSTALL_ADVICE}")?;
        }
        UpdateOutcome::SourceInstall(SourceInstallDetail::NoRelease { target }) => {
            writeln!(out, "No qualifying release found for {target}.")?;
            writeln!(out, "{SOURCE_INSTALL_ADVICE}")?;
        }
        UpdateOutcome::UpToDate => writeln!(out, "patok is up to date.")?,
        UpdateOutcome::Available { release, .. } => {
            writeln!(out, "New release available: {}", release.tag)?
        }
    }
    Ok(())
}

fn install(
    client: &dyn HttpClient,
    dir: &Path,
    release: &Release,
    asset: &Asset,
    exe: &Path,
) -> anyhow::Result<()> {
    let archive = download_verified(client, release, asset, dir)?;
    let extract_dir = dir.join("extracted");
    std::fs::create_dir_all(&extract_dir)?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&extract_dir)
        .status()
        .context("cannot run tar")?;
    if !status.success() {
        bail!("failed to extract {}", asset.name);
    }
    let new_binary = find_binary(&extract_dir)
        .with_context(|| format!("no patok binary found in {}", asset.name))?;
    replace_executable(exe, &new_binary).context("failed to replace the running executable")?;
    clear_quarantine(exe);
    Ok(())
}

/// Find the file named `patok` anywhere under `dir`.
fn find_binary(dir: &Path) -> Option<std::path::PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_binary(&path) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == "patok") {
            return Some(path);
        }
    }
    None
}

/// Remove the macOS quarantine attribute; failures (e.g. attribute absent) are ignored.
fn clear_quarantine(exe: &Path) {
    if cfg!(target_os = "macos") {
        let _ = Command::new("xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(exe)
            .output();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct FakeSource {
        releases: Result<String, HttpError>,
        files: HashMap<String, Vec<u8>>,
        calls: RefCell<Vec<(String, Duration)>>,
    }

    impl HttpClient for FakeSource {
        fn get(&self, _url: &str, timeout: Duration) -> Result<String, HttpError> {
            self.calls.borrow_mut().push((_url.to_string(), timeout));
            self.releases.clone()
        }

        fn download(&self, url: &str, dest: &Path, timeout: Duration) -> Result<(), HttpError> {
            self.calls.borrow_mut().push((url.to_string(), timeout));
            let name = url.rsplit('/').next().unwrap();
            let body = self.files.get(name).ok_or(HttpError("404".into()))?;
            std::fs::write(dest, body).map_err(|e| HttpError(e.to_string()))
        }
    }

    const ARCHIVE: &str = "patok-x86_64-unknown-linux-musl.tar.gz";

    /// A release source serving a real `.tar.gz` holding a `patok` file with `payload`.
    fn source(tag: &str, payload: &str, checksum_override: Option<&str>) -> FakeSource {
        let work = tempfile::tempdir().unwrap();
        let pkg = work.path().join("pkg");
        std::fs::create_dir(&pkg).unwrap();
        std::fs::write(pkg.join("patok"), payload).unwrap();
        let archive = work.path().join(ARCHIVE);
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&pkg)
            .arg("patok")
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = std::fs::read(&archive).unwrap();
        let hash = checksum_override.map(str::to_string).unwrap_or_else(|| {
            let out = Command::new("sha256sum").arg(&archive).output().unwrap();
            String::from_utf8(out.stdout)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .to_string()
        });
        let releases = serde_json::json!([{
            "tag_name": tag,
            "assets": [
                {"name": ARCHIVE, "browser_download_url": format!("https://x/{tag}/{ARCHIVE}")},
                {"name": "checksums", "browser_download_url": format!("https://x/{tag}/checksums")},
            ],
        }]);
        FakeSource {
            releases: Ok(releases.to_string()),
            files: HashMap::from([
                (ARCHIVE.to_string(), bytes),
                (
                    "checksums".to_string(),
                    format!("{hash}  {ARCHIVE}\n").into_bytes(),
                ),
            ]),
            calls: RefCell::default(),
        }
    }

    /// A release source whose listing is `releases` and whose downloads 404.
    fn listing_source(releases: Result<String, HttpError>) -> FakeSource {
        FakeSource {
            releases,
            files: HashMap::default(),
            calls: RefCell::default(),
        }
    }

    fn update(source: &FakeSource, exe: &Path) -> (anyhow::Result<()>, String) {
        let mut out = Vec::new();
        let result = run_with(
            &mut out,
            source,
            UpdateChannel::Stable,
            "1.0.0",
            exe,
            "linux",
            "x86_64",
        );
        (result, String::from_utf8(out).unwrap())
    }

    #[test]
    fn update_downloads_verifies_and_replaces_the_executable() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("patok");
        std::fs::write(&exe, "old binary").unwrap();
        let (result, out) = update(&source("v9.0.1", "new binary", None), &exe);
        result.unwrap();
        assert!(out.contains("New release available: v9.0.1"), "{out}");
        assert!(out.contains("patok updated to v9.0.1"), "{out}");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new binary");
        assert!(!dir.path().join("patok.bak").exists());
        assert!(!update_temp_dir("v9.0.1").exists());
    }

    #[test]
    fn update_with_a_bad_checksum_leaves_the_executable_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("patok");
        std::fs::write(&exe, "old binary").unwrap();
        let bad = "0".repeat(64);
        let (result, _) = update(&source("v9.0.2", "new binary", Some(&bad)), &exe);
        assert!(format!("{:#}", result.unwrap_err()).contains("checksum mismatch"));
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old binary");
        assert!(!update_temp_dir("v9.0.2").exists());
    }

    #[test]
    fn update_when_current_replaces_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("patok");
        std::fs::write(&exe, "old binary").unwrap();
        let (result, out) = update(&source("v1.0.0", "new binary", None), &exe);
        result.unwrap();
        assert!(out.contains("up to date"), "{out}");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "old binary");
    }

    #[test]
    fn update_homebrew_prints_brew_advice_without_lookup() {
        let source = listing_source(Err(HttpError("unreachable".into())));
        let mut out = Vec::new();
        run_with(
            &mut out,
            &source,
            UpdateChannel::Stable,
            "1.0.0",
            Path::new("/opt/homebrew/Cellar/patok/1.0.0/bin/patok"),
            "macos",
            "aarch64",
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.starts_with("patok 1.0.0\n"), "{out}");
        assert!(out.contains("brew upgrade"), "{out}");
        assert!(source.calls.borrow().is_empty());
    }

    #[test]
    fn update_unsupported_platform_prints_source_advice() {
        let source = listing_source(Err(HttpError("unreachable".into())));
        let mut out = Vec::new();
        run_with(
            &mut out,
            &source,
            UpdateChannel::Stable,
            "1.0.0",
            Path::new("/usr/bin/patok"),
            "linux",
            "aarch64",
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("No release is published for linux/aarch64."),
            "{out}"
        );
        assert!(out.contains("Build from source"), "{out}");
    }

    #[test]
    fn update_no_qualifying_release_prints_source_advice() {
        let source = listing_source(Ok("[]".into()));
        let mut out = Vec::new();
        run_with(
            &mut out,
            &source,
            UpdateChannel::Stable,
            "1.0.0",
            Path::new("/x/patok"),
            "linux",
            "x86_64",
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("No qualifying release found for x86_64-unknown-linux-musl."),
            "{out}"
        );
        assert!(out.contains("Build from source"), "{out}");
    }

    #[test]
    fn update_lookup_failure_prints_source_advice() {
        let source = listing_source(Err(HttpError("down".into())));
        let mut out = Vec::new();
        run_with(
            &mut out,
            &source,
            UpdateChannel::Stable,
            "1.0.0",
            Path::new("/x/patok"),
            "linux",
            "x86_64",
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("Could not look up releases:"), "{out}");
        assert!(out.contains("Build from source"), "{out}");
    }

    // sha256("hello")
    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn download_fixture(checksums: &str, body: &[u8]) -> (FakeSource, Release, Asset) {
        let mut source = listing_source(Ok("[]".into()));
        source.files.insert(ARCHIVE.into(), body.to_vec());
        source.files.insert("checksums".into(), checksums.into());
        let release = Release {
            tag: "v1".into(),
            archives: vec![],
            checksums_url: "https://x/v1/checksums".into(),
        };
        let asset = Asset {
            name: ARCHIVE.into(),
            url: format!("https://x/v1/{ARCHIVE}"),
        };
        (source, release, asset)
    }

    #[test]
    fn download_verifies_and_uses_cap() {
        let (source, rel, asset) = download_fixture(&format!("{HELLO}  {ARCHIVE}\n"), b"hello");
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("v1");
        let p = download_verified(&source, &rel, &asset, &d).unwrap();
        assert_eq!(std::fs::read(p).unwrap(), b"hello");
        assert!(
            source
                .calls
                .borrow()
                .iter()
                .all(|c| c.1 == DOWNLOAD_TIMEOUT)
        );
    }

    #[test]
    fn download_aborts_on_mismatch_and_missing_line() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().join("v1");
        let (source, rel, asset) = download_fixture(&format!("{HELLO}  {ARCHIVE}\n"), b"tampered");
        let err = download_verified(&source, &rel, &asset, &d).unwrap_err();
        assert!(matches!(err, DownloadError::Mismatch { .. }));
        assert!(!d.exists());
        let (source, rel, asset) = download_fixture(&format!("{HELLO}  x{ARCHIVE}\n"), b"hello");
        let err = download_verified(&source, &rel, &asset, &d).unwrap_err();
        assert!(matches!(err, DownloadError::MissingChecksum(_)));
        assert!(!d.exists());
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

    /// A user config layer whose `tui.update_channel` is `dev`, with the project
    /// layer skipped (`None` data dir).
    fn dev_channel_files() -> (tempfile::TempDir, ConfigFiles) {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config.toml");
        std::fs::write(&user, "[tui]\nupdate_channel = \"dev\"\n").unwrap();
        let files = ConfigFiles {
            user_global: Some(user),
            user_local: None,
        };
        (dir, files)
    }

    #[test]
    fn the_channel_flag_wins_over_the_config() {
        let (dir, files) = dev_channel_files();
        let (channel, warnings) =
            resolve_channel(Some(ChannelArg::Stable), &files, dir.path(), None);
        assert_eq!(channel, UpdateChannel::Stable);
        assert!(warnings.is_empty(), "{warnings:?}");

        // And the other direction: a dev flag over a stable config.
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config.toml");
        std::fs::write(&user, "[tui]\nupdate_channel = \"stable\"\n").unwrap();
        let files = ConfigFiles {
            user_global: Some(user),
            user_local: None,
        };
        let (channel, _) = resolve_channel(Some(ChannelArg::Dev), &files, dir.path(), None);
        assert_eq!(channel, UpdateChannel::Dev);
    }

    #[test]
    fn the_configured_channel_applies_without_the_flag() {
        let (dir, files) = dev_channel_files();
        let (channel, warnings) = resolve_channel(None, &files, dir.path(), None);
        assert_eq!(channel, UpdateChannel::Dev);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn the_default_channel_applies_without_flag_or_config() {
        let dir = tempfile::tempdir().unwrap();
        let (channel, warnings) = resolve_channel(None, &ConfigFiles::default(), dir.path(), None);
        assert_eq!(channel, UpdateChannel::Stable);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn the_cli_channel_spellings_stay_stable_and_dev() {
        use clap::ValueEnum;
        assert_eq!(
            ChannelArg::Stable.to_possible_value().unwrap().get_name(),
            "stable"
        );
        assert_eq!(
            ChannelArg::Dev.to_possible_value().unwrap().get_name(),
            "dev"
        );
        assert_eq!(
            UpdateChannel::from(ChannelArg::Stable),
            UpdateChannel::Stable
        );
        assert_eq!(UpdateChannel::from(ChannelArg::Dev), UpdateChannel::Dev);
    }
}
