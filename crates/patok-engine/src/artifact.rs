//! Timestamped artifact files in the current task directory (the session directory
//! outside a task): the research, plan, claims and review stages write `<prefix>-<unix seconds>.md`
//! files and scan the newest one back on restart.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Time elapsed since the unix epoch; `Duration::ZERO` if the clock is before it.
pub(crate) fn unix_now() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// Why an artifact write failed, with the path the file was meant to take.
#[derive(Debug)]
pub(crate) struct WriteArtifactError {
    pub path: PathBuf,
    pub error: io::Error,
}

/// Writes `contents` to `<dir>/<prefix>-<unix seconds>.md`, creating `dir`,
/// and returns the path. Plain `fs::write`, like the stages did before.
pub(crate) fn write_artifact(
    dir: &Path,
    prefix: &str,
    contents: &str,
) -> Result<PathBuf, WriteArtifactError> {
    let path = dir.join(format!("{prefix}-{}.md", unix_now().as_secs()));
    let written = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&path, contents));
    match written {
        Ok(()) => Ok(path),
        Err(error) => Err(WriteArtifactError { path, error }),
    }
}

/// The filename middle between `<prefix>-` and `.md`, when the name has the shape.
fn middle<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(".md"))
}

/// The filename timestamp of an artifact, when the middle parses as seconds.
fn timestamp(name: &str, prefix: &str) -> Option<u64> {
    middle(name, prefix).and_then(|secs| secs.parse().ok())
}

/// The newest `<prefix>-*.md` artifact in `dir`.
///
/// `Some(max_age)` keeps only names whose middle parses as seconds within
/// `max_age` of now and picks the largest timestamp (research reuse);
/// `None` accepts any middle and picks the lexicographically largest
/// name (build claims).
pub(crate) fn newest_artifact(dir: &Path, prefix: &str, max_age: Option<u64>) -> Option<PathBuf> {
    let prefix = format!("{prefix}-");
    let now = unix_now().as_secs();
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| middle(name, &prefix).is_some())
        .filter(|name| match max_age {
            None => true,
            Some(max) => timestamp(name, &prefix).is_some_and(|t| now.saturating_sub(t) < max),
        })
        .max_by(|a, b| match max_age {
            None => a.cmp(b),
            Some(_) => timestamp(a, &prefix).cmp(&timestamp(b, &prefix)),
        })
        .map(|name| dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_artifact_names_and_writes_the_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_artifact(dir.path(), "T1.1", "the contents").unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        let middle = name
            .strip_prefix("T1.1-")
            .and_then(|rest| rest.strip_suffix(".md"))
            .unwrap();
        assert!(!middle.is_empty());
        assert!(middle.bytes().all(|byte| byte.is_ascii_digit()));
        assert_eq!(path.parent().unwrap(), dir.path());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "the contents");
    }

    #[test]
    fn newest_artifact_without_age_picks_the_lexicographically_largest_name() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "T1.1-100.md",
            "T1.1-200.md",
            "T1.1-99.md",
            "T2.1-300.md",
            "T1.1-notes.md",
            "notes.txt",
        ] {
            std::fs::write(dir.path().join(name), "seed").unwrap();
        }
        let newest = newest_artifact(dir.path(), "T1.1", None).unwrap();
        assert_eq!(newest, dir.path().join("T1.1-notes.md"));
        // Without the non-numeric name the pick is still lexicographic,
        // not numeric: `99` beats `200`.
        std::fs::remove_file(dir.path().join("T1.1-notes.md")).unwrap();
        let newest = newest_artifact(dir.path(), "T1.1", None).unwrap();
        assert_eq!(newest, dir.path().join("T1.1-99.md"));
    }

    #[test]
    fn newest_artifact_with_age_picks_the_freshest_parsing_timestamp() {
        let now = unix_now().as_secs();
        let dir = tempfile::tempdir().unwrap();
        for name in [
            format!("T1.1-{}.md", now),
            format!("T1.1-{}.md", now - 601),
            "T1.1-old.md".to_string(),
        ] {
            std::fs::write(dir.path().join(name), "seed").unwrap();
        }
        let newest = newest_artifact(dir.path(), "T1.1", Some(600)).unwrap();
        assert_eq!(newest, dir.path().join(format!("T1.1-{now}.md")));
        // A dir holding only the stale report reuses nothing.
        let stale = tempfile::tempdir().unwrap();
        std::fs::write(stale.path().join(format!("T1.1-{}.md", now - 601)), "seed").unwrap();
        assert_eq!(newest_artifact(stale.path(), "T1.1", Some(600)), None);
    }

    #[test]
    fn newest_artifact_in_empty_or_missing_directories_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(newest_artifact(dir.path(), "T1.1", None), None);
        assert_eq!(newest_artifact(dir.path(), "T1.1", Some(600)), None);
        let missing = dir.path().join("missing");
        assert_eq!(newest_artifact(&missing, "T1.1", None), None);
        assert_eq!(newest_artifact(&missing, "T1.1", Some(600)), None);
    }
}
