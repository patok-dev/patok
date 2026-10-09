//! The one atomic file-write helper shared by every persistence path (finding 5 in
//! docs/private/structure-review.md): config persistence, the task file and the review
//! history all write through here, so a settings change, a task-file rewrite and a
//! history save share the same crash-safety and temp-file hygiene.

use std::io::Write;
use std::path::Path;

/// Writes `contents` to `path` atomically: the data lands in a uniquely named temporary
/// file in the target's directory (the unique part keeps concurrent writers -- the engine
/// process and the `patok` shell both rewrite the task file -- from colliding on one
/// temp name), is flushed, and the temporary is renamed over the target, so a failed
/// write leaves the target's previous contents intact and no reader ever sees a partial
/// file. Missing parent directories are created first; on any failure the temporary file
/// is removed best-effort.
pub fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.to_path_buf();
    let unique = std::process::id()
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
    tmp.set_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("patok"),
        unique
    ));
    let result = std::fs::File::create(&tmp)
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())?;
            file.flush()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_replaces_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "old").unwrap();
        write_atomic(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn write_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        write_atomic(&path, "# Tasks\n").unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Tasks\n",
            "the directory's one entry is the target itself"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_write_leaves_previous_contents_intact() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, "previous").unwrap();
        // A read-only parent directory makes the temporary file's creation fail.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let error = write_atomic(&path, "new").unwrap_err();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "previous");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temp file was left behind"
        );
    }

    #[test]
    fn write_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("state.json");
        write_atomic(&path, "{}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
    }
}
