//! Per-project identity and directory layout.

use std::path::{Path, PathBuf};

/// Escapes an absolute path into one directory name: double every literal `-` in each
/// component, then join components with `-`, with a leading `-` for the root.
/// `/home/user-x/proj` becomes `-home-user--x-proj`.
pub fn escape_path(path: &Path) -> String {
    path.to_string_lossy()
        .split('/')
        .map(|component| component.replace('-', "--"))
        .collect::<Vec<_>>()
        .join("-")
}

/// Longest socket path we allow. `sun_path` holds 104 bytes on macOS and 108 on Linux, including
/// the terminating NUL.
const MAX_SOCKET_PATH: usize = 100;
const SOCKET_FILE: &str = "engine.sock";

/// Runtime directory of the engine for `project`: `<root>/patok/<escaped-cwd>`, where `<root>`
/// is `$XDG_RUNTIME_DIR`, else `$TMPDIR`, else `/tmp`.
///
/// Deep project paths would push the socket path past the OS limit. In that case (only) the
/// directory name keeps a readable tail of the escaped path and ends in a stable 64-bit hash of
/// the whole of it, so distinct projects still get distinct names.
pub fn runtime_dir(xdg_runtime_dir: Option<&str>, tmpdir: Option<&str>, project: &Path) -> PathBuf {
    let root = [xdg_runtime_dir, tmpdir]
        .into_iter()
        .flatten()
        .find(|v| !v.is_empty())
        .unwrap_or("/tmp");
    let base = Path::new(root).join("patok");
    let escaped = escape_path(project);
    // "<base>/<name>/<socket file>"
    let overhead = base.as_os_str().len() + 1 + 1 + SOCKET_FILE.len();
    let budget = MAX_SOCKET_PATH.saturating_sub(overhead);
    let name = if escaped.len() <= budget {
        escaped
    } else {
        shorten(&escaped, budget)
    };
    base.join(name)
}

/// `<tail>-<hash>` within `budget` bytes (the hash alone when there is no room for a tail).
fn shorten(escaped: &str, budget: usize) -> String {
    let hash = format!("{:016x}", fnv1a(escaped.as_bytes()));
    let room = budget.saturating_sub(hash.len() + 1);
    let mut start = escaped.len().saturating_sub(room);
    while !escaped.is_char_boundary(start) {
        start += 1;
    }
    let tail = &escaped[start..];
    if tail.is_empty() {
        hash
    } else {
        format!("{tail}-{hash}")
    }
}

/// FNV-1a, 64 bit: simple and, unlike `DefaultHasher`, stable across Rust releases and processes.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Persistent per-project directory: `<data>/patok/projects/<id>`, where `<data>` is
/// `$XDG_DATA_HOME`, else `$HOME/.local/share`.
pub fn project_data_dir(
    xdg_data_home: Option<&str>,
    home: Option<&str>,
    project: &Path,
) -> Option<PathBuf> {
    let root = match xdg_data_home.filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(home.filter(|v| !v.is_empty())?).join(".local/share"),
    };
    Some(
        root.join("patok")
            .join("projects")
            .join(escape_path(project)),
    )
}

/// [`runtime_dir`] from the process environment.
pub fn runtime_dir_from_env(project: &Path) -> PathBuf {
    let get = |k: &str| std::env::var(k).ok();
    runtime_dir(
        get("XDG_RUNTIME_DIR").as_deref(),
        get("TMPDIR").as_deref(),
        project,
    )
}

/// [`project_data_dir`] from the process environment.
pub fn project_data_dir_from_env(project: &Path) -> Option<PathBuf> {
    let get = |k: &str| std::env::var(k).ok();
    project_data_dir(
        get("XDG_DATA_HOME").as_deref(),
        get("HOME").as_deref(),
        project,
    )
}

/// The engine socket inside a runtime directory.
pub fn socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(SOCKET_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_dashes_and_separators() {
        assert_eq!(
            escape_path(Path::new("/home/user-x/proj")),
            "-home-user--x-proj"
        );
        assert_eq!(escape_path(Path::new("/")), "-");
    }

    #[test]
    fn runtime_dir_falls_back() {
        let p = Path::new("/a/b");
        assert_eq!(
            runtime_dir(Some("/run/user/1"), Some("/t"), p),
            Path::new("/run/user/1/patok/-a-b")
        );
        assert_eq!(runtime_dir(None, Some("/t"), p), Path::new("/t/patok/-a-b"));
        assert_eq!(runtime_dir(Some(""), None, p), Path::new("/tmp/patok/-a-b"));
    }

    #[test]
    fn deep_paths_get_a_hashed_name_that_fits_the_socket_limit() {
        let deep = |leaf: &str| {
            Path::new("/home/user/projects/some/deeply/nested/workspace/with/many/levels/of/dirs")
                .join(leaf)
        };
        let a = runtime_dir(Some("/run/user/1000"), None, &deep("a"));
        let b = runtime_dir(Some("/run/user/1000"), None, &deep("b"));
        assert_ne!(a, b);
        assert!(
            socket_path(&a).as_os_str().len() <= MAX_SOCKET_PATH,
            "{a:?}"
        );
        assert!(
            a.file_name().unwrap().to_string_lossy().contains("-a-"),
            "keeps a readable tail: {a:?}"
        );
        // Stable: the same project always maps to the same directory.
        assert_eq!(a, runtime_dir(Some("/run/user/1000"), None, &deep("a")));
    }

    #[test]
    fn hash_alone_when_the_root_leaves_no_room_for_a_tail() {
        let root = format!("/{}", "r".repeat(70));
        let dir = runtime_dir(
            Some(&root),
            None,
            Path::new("/a/very/long/project/path/that/does/not/fit/anywhere"),
        );
        assert!(dir.file_name().unwrap().len() >= 16);
    }

    #[test]
    fn data_dir_falls_back_to_home() {
        let p = Path::new("/a");
        assert_eq!(
            project_data_dir(None, Some("/home/u"), p).unwrap(),
            Path::new("/home/u/.local/share/patok/projects/-a")
        );
        assert!(project_data_dir(None, None, p).is_none());
    }
}
