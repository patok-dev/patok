//! The one atomic file-write helper shared by every persistence path (finding 5 in
//! docs/private/structure-review.md): config persistence, the task file and the review
//! history all write through here, so a settings change, a task-file rewrite and a
//! history save share the same crash-safety and temp-file hygiene.

use std::io::Write;
use std::path::Path;

/// The suffix of every temp file name ([`write_atomic`] and the sweep agree on it).
const TEMP_SUFFIX: &str = ".tmp";

/// The prefix every temp file of `file_name` starts with: `.<file_name>.`,
/// so a temp sibling of `TASKS.md` is `.TASKS.md.<unique>.tmp`.
fn temp_prefix(file_name: &str) -> String {
    format!(".{file_name}.")
}

/// The file name of `target`, or `patok` when it is not valid UTF-8.
fn file_name_or_default(target: &Path) -> &str {
    target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("patok")
}

/// The glob matching every temp sibling of `target` -- `.TASKS.md.*.tmp` for a
/// task file -- the same names [`write_atomic`] creates next to it and
/// [`sweep_temp_files`] removes. A raw glob, no git pathspec magic: the
/// `:(exclude)` wrapping belongs to the git layer.
pub fn temp_glob(target: &Path) -> String {
    format!(
        "{}*{TEMP_SUFFIX}",
        temp_prefix(file_name_or_default(target))
    )
}

/// Is `name` one of `target`'s temp siblings: the [`temp_prefix`] of the
/// target's name, a non-empty unique part, then [`TEMP_SUFFIX`]? A plain
/// `.TASKS.md.tmp` (no unique part) is not: `write_atomic` never creates it.
fn is_temp_sibling(name: &str, prefix: &str) -> bool {
    name.starts_with(prefix)
        && name.ends_with(TEMP_SUFFIX)
        && name.len() > prefix.len() + TEMP_SUFFIX.len()
}

/// Removes the stale temp siblings of `target` -- files a hard kill between
/// [`write_atomic`]'s create and rename leaves behind -- and returns how many
/// were removed. A missing directory is not an error (the target's parent may
/// not exist yet); a file that cannot be removed is skipped, not an error.
///
/// The accepted race: a concurrently live temp of a writer that does not hold
/// the engine's task-file lock (the `patok` shell also rewrites the task
/// file) can be removed; that writer's rename then fails and its own error
/// handling takes over. The engine's writers hold the same lock the sweep
/// runs under, so they are safe.
pub fn sweep_temp_files(target: &Path) -> std::io::Result<usize> {
    let Some(dir) = target.parent() else {
        return Ok(0);
    };
    let prefix = temp_prefix(file_name_or_default(target));
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_temp_sibling(name, &prefix) {
            let path = dir.join(name);
            if std::fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

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
        "{}{unique}{TEMP_SUFFIX}",
        temp_prefix(file_name_or_default(path))
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

    #[test]
    fn temp_glob_matches_the_task_file_pattern() {
        assert_eq!(temp_glob(Path::new("TASKS.md")), ".TASKS.md.*.tmp");
        assert_eq!(
            temp_glob(Path::new("/somewhere/state.json")),
            ".state.json.*.tmp"
        );
    }

    #[test]
    fn sweep_removes_only_temp_siblings_with_a_unique_part() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("TASKS.md");
        std::fs::write(&path, "# Tasks\n").unwrap();
        // The unique part is `pid ^ nanos`, not digits: any non-empty text matches.
        for name in [".TASKS.md.1.tmp", ".TASKS.md.abc.tmp"] {
            std::fs::write(dir.path().join(name), "orphan").unwrap();
        }
        // No unique part, so not a name write_atomic ever creates.
        std::fs::write(dir.path().join(".TASKS.md.tmp"), "kept").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "kept").unwrap();
        assert_eq!(sweep_temp_files(&path).unwrap(), 2);
        assert!(!dir.path().join(".TASKS.md.1.tmp").exists());
        assert!(!dir.path().join(".TASKS.md.abc.tmp").exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# Tasks\n");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".TASKS.md.tmp")).unwrap(),
            "kept"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
            "kept"
        );
    }

    #[test]
    fn sweep_of_a_missing_directory_removes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent").join("TASKS.md");
        assert_eq!(sweep_temp_files(&path).unwrap(), 0);
    }
}
