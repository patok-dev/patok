//! Discovery of running engines: one runtime directory per project under the runtime root.

use std::path::{Path, PathBuf};

use crate::paths::{DaemonMeta, meta_path, socket_path};

/// An engine found by [`scan`]. The socket may be stale; discovery only checks that it exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetEntry {
    /// `<root>/<runtime dir name>/engine.sock`.
    pub socket_path: PathBuf,
    /// Parsed `meta.json`; `None` when missing, unreadable or malformed.
    pub meta: Option<DaemonMeta>,
    /// The runtime directory's file name (the escaped, possibly hashed project path).
    pub runtime_dir_name: String,
}

/// Lists `<root>/*/engine.sock`, where `root` is the runtime root (see
/// [`crate::paths::runtime_root`]), sorted by runtime dir name. A missing root is an
/// empty fleet. Children that are not directories, or have no socket, are skipped.
pub fn scan(root: &Path) -> std::io::Result<Vec<FleetEntry>> {
    let read_dir = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut entries = Vec::new();
    for entry in read_dir {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let dir = entry.path();
        let sock = socket_path(&dir);
        if !sock.exists() {
            continue;
        }
        entries.push(FleetEntry {
            socket_path: sock,
            meta: DaemonMeta::read(&meta_path(&dir)).ok(),
            runtime_dir_name: entry.file_name().to_string_lossy().into_owned(),
        });
    }
    entries.sort_by(|a, b| a.runtime_dir_name.cmp(&b.runtime_dir_name));
    Ok(entries)
}

/// [`scan`] of [`crate::paths::runtime_root_from_env`].
pub fn scan_from_env() -> std::io::Result<Vec<FleetEntry>> {
    scan(&crate::paths::runtime_root_from_env())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta(pid: u32) -> DaemonMeta {
        DaemonMeta {
            project_dir: PathBuf::from("/home/u/proj"),
            pid,
            started_at: 1_700_000_000,
            engine_version: "1.2.3".into(),
        }
    }

    fn project(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch_socket(dir: &Path) {
        std::fs::write(socket_path(dir), "").unwrap();
    }

    fn root_in(tmp: &tempfile::TempDir) -> PathBuf {
        let root = tmp.path().join("patok");
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn missing_root_is_an_empty_fleet() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(scan(&tmp.path().join("patok")).unwrap(), vec![]);
    }

    #[test]
    fn skips_project_dirs_without_a_socket_and_stray_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(&tmp);
        let dir = project(&root, "-a-b");
        sample_meta(1).write(&meta_path(&dir)).unwrap();
        std::fs::write(root.join("stray"), "").unwrap();
        assert_eq!(scan(&root).unwrap(), vec![]);
    }

    #[test]
    fn socket_without_meta_has_no_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(&tmp);
        touch_socket(&project(&root, "-a-b"));
        let entries = scan(&root).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].socket_path,
            root.join("-a-b").join("engine.sock")
        );
        assert_eq!(entries[0].meta, None);
        assert_eq!(entries[0].runtime_dir_name, "-a-b");
    }

    #[test]
    fn socket_with_meta_carries_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(&tmp);
        let dir = project(&root, "-a-b");
        touch_socket(&dir);
        sample_meta(42).write(&meta_path(&dir)).unwrap();
        let entries = scan(&root).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].meta, Some(sample_meta(42)));
    }

    #[test]
    fn malformed_meta_still_lists_the_daemon_without_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(&tmp);
        let dir = project(&root, "-a-b");
        touch_socket(&dir);
        std::fs::write(meta_path(&dir), "not json").unwrap();
        let entries = scan(&root).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].meta, None);
    }

    #[test]
    fn entries_are_sorted_by_runtime_dir_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = root_in(&tmp);
        touch_socket(&project(&root, "-z"));
        touch_socket(&project(&root, "-a"));
        let names: Vec<_> = scan(&root)
            .unwrap()
            .into_iter()
            .map(|e| e.runtime_dir_name)
            .collect();
        assert_eq!(names, ["-a", "-z"]);
    }
}
