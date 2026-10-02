//! `patok update`: check for a newer release.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, bail};
use patok_core::update::{
    Channel, HttpClient, HttpError, UpdateOutcome, check_update, download_verified,
    replace_executable, update_temp_dir,
};

/// HTTP client that shells out to `curl`.
struct CurlClient;

impl HttpClient for CurlClient {
    fn get(&self, url: &str, timeout: Duration) -> Result<String, HttpError> {
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--fail", "--location"])
            .args(["--max-time", &timeout.as_secs().to_string()])
            .args(["--header", "Accept: application/vnd.github+json"])
            .args(["--header", "User-Agent: patok"])
            .arg(url)
            .output()
            .map_err(|e| HttpError(format!("cannot run curl: {e}")))?;
        if !output.status.success() {
            return Err(HttpError(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        String::from_utf8(output.stdout).map_err(|e| HttpError(e.to_string()))
    }

    fn download(&self, url: &str, dest: &Path, timeout: Duration) -> Result<(), HttpError> {
        let output = Command::new("curl")
            .args(["--silent", "--show-error", "--fail", "--location"])
            .args(["--max-time", &timeout.as_secs().to_string()])
            .args(["--header", "User-Agent: patok"])
            .arg("--output")
            .arg(dest)
            .arg(url)
            .output()
            .map_err(|e| HttpError(format!("cannot run curl: {e}")))?;
        if !output.status.success() {
            return Err(HttpError(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(())
    }
}

/// The `--channel` values of `patok update`.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum ChannelArg {
    Stable,
    Dev,
}

impl From<ChannelArg> for Channel {
    fn from(arg: ChannelArg) -> Self {
        match arg {
            ChannelArg::Stable => Channel::Stable,
            ChannelArg::Dev => Channel::Dev,
        }
    }
}

pub fn run(channel: Channel) -> anyhow::Result<()> {
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
    channel: Channel,
    current: &str,
    exe: &Path,
    os: &str,
    arch: &str,
) -> anyhow::Result<()> {
    let outcome = check_update(out, client, channel, current, exe, os, arch)?;
    if let UpdateOutcome::Available { release, asset } = outcome {
        let dir = update_temp_dir(&release.tag);
        let result = install(client, &dir, &release, &asset, exe);
        let _ = std::fs::remove_dir_all(&dir);
        result?;
        writeln!(out, "patok updated to {}", release.tag)?;
    }
    Ok(())
}

fn install(
    client: &dyn HttpClient,
    dir: &Path,
    release: &patok_core::update::Release,
    asset: &patok_core::update::Asset,
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
    use std::collections::HashMap;

    struct FakeSource {
        releases: String,
        files: HashMap<String, Vec<u8>>,
    }

    impl HttpClient for FakeSource {
        fn get(&self, _url: &str, _timeout: Duration) -> Result<String, HttpError> {
            Ok(self.releases.clone())
        }

        fn download(&self, url: &str, dest: &Path, _timeout: Duration) -> Result<(), HttpError> {
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
            releases: releases.to_string(),
            files: HashMap::from([
                (ARCHIVE.to_string(), bytes),
                (
                    "checksums".to_string(),
                    format!("{hash}  {ARCHIVE}\n").into_bytes(),
                ),
            ]),
        }
    }

    fn update(source: &FakeSource, exe: &Path) -> (anyhow::Result<()>, String) {
        let mut out = Vec::new();
        let result = run_with(
            &mut out,
            source,
            Channel::Stable,
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
}
