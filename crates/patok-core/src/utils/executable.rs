//! PATH lookups for provider executables.

use std::ffi::OsStr;

/// Whether an executable called `name` is in one of the directories of `path`.
pub fn cli_in(name: &str, path: Option<&OsStr>) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_some_and(|path| {
        std::env::split_paths(path).any(|dir| {
            std::fs::metadata(dir.join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn path_of(dir: &std::path::Path) -> std::ffi::OsString {
        std::env::join_paths([dir]).unwrap()
    }

    #[test]
    fn an_executable_file_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("tool");
        std::fs::write(&file, "").unwrap();
        std::fs::set_permissions(&file, PermissionsExt::from_mode(0o755)).unwrap();
        assert!(cli_in("tool", Some(&path_of(dir.path()))));
    }

    #[test]
    fn a_directory_or_missing_name_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("tool")).unwrap();
        let path = path_of(dir.path());
        assert!(!cli_in("tool", Some(&path)));
        assert!(!cli_in("other", Some(&path)));
    }

    #[test]
    fn a_missing_path_finds_nothing() {
        assert!(!cli_in("tool", None));
    }
}
