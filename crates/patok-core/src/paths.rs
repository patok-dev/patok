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

/// The env-injectable base-directory resolution shared by the runtime dir, the
/// project data dir, the user config layers, the user cache dir and the default
/// review history:
/// `primary` when set and non-empty, else `fallback` (set and non-empty) joined
/// with `fallback_subdir` (used as-is when the subdir is empty). Empty values
/// count as unset; `None` when neither resolves.
pub fn base_dir(
    primary: Option<&str>,
    fallback: Option<&str>,
    fallback_subdir: &str,
) -> Option<PathBuf> {
    if let Some(dir) = primary.filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let base = PathBuf::from(fallback.filter(|v| !v.is_empty())?);
    Some(if fallback_subdir.is_empty() {
        base
    } else {
        base.join(fallback_subdir)
    })
}

/// Longest socket path we allow. `sun_path` holds 104 bytes on macOS and 108 on Linux, including
/// the terminating NUL.
const MAX_SOCKET_PATH: usize = 100;
const SOCKET_FILE: &str = "engine.sock";

const META_FILE: &str = "meta.json";

/// Root holding every project's runtime directory: `<base>/patok`, where `<base>` is
/// `$XDG_RUNTIME_DIR`, else `$TMPDIR`, else `/tmp` (empty values count as unset).
pub fn runtime_root(xdg_runtime_dir: Option<&str>, tmpdir: Option<&str>) -> PathBuf {
    base_dir(xdg_runtime_dir, tmpdir, "")
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("patok")
}

/// Runtime directory of the engine for `project`: `<runtime root>/<escaped-cwd>` (see
/// [`runtime_root`]).
///
/// Deep project paths would push the socket path past the OS limit. In that case (only) the
/// directory name keeps a readable tail of the escaped path and ends in a stable 64-bit hash of
/// the whole of it, so distinct projects still get distinct names.
pub fn runtime_dir(xdg_runtime_dir: Option<&str>, tmpdir: Option<&str>, project: &Path) -> PathBuf {
    let base = runtime_root(xdg_runtime_dir, tmpdir);
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
    let root = base_dir(xdg_data_home, home, ".local/share")?;
    Some(
        root.join("patok")
            .join("projects")
            .join(escape_path(project)),
    )
}

/// [`runtime_root`] from the process environment.
pub fn runtime_root_from_env() -> PathBuf {
    let get = |k: &str| std::env::var(k).ok();
    runtime_root(get("XDG_RUNTIME_DIR").as_deref(), get("TMPDIR").as_deref())
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

/// The patok directory under the user's XDG config dir: `$XDG_CONFIG_HOME`,
/// else `$HOME/.config`, each joined with `patok`. Empty values count as
/// unset; `None` when neither resolves.
pub fn config_dir(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    Some(base_dir(xdg_config_home, home, ".config")?.join("patok"))
}

/// [`config_dir`] from the process environment.
pub fn config_dir_from_env() -> Option<PathBuf> {
    let get = |k: &str| std::env::var(k).ok();
    config_dir(get("XDG_CONFIG_HOME").as_deref(), get("HOME").as_deref())
}

/// The patok directory under the user's XDG cache dir: `$XDG_CACHE_HOME`,
/// else `$HOME/.cache`, each joined with `patok`. Empty values count as
/// unset; `None` when neither resolves.
pub fn cache_dir(xdg_cache_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    Some(base_dir(xdg_cache_home, home, ".cache")?.join("patok"))
}

/// [`cache_dir`] from the process environment.
pub fn cache_dir_from_env() -> Option<PathBuf> {
    let get = |k: &str| std::env::var(k).ok();
    cache_dir(get("XDG_CACHE_HOME").as_deref(), get("HOME").as_deref())
}

/// The engine socket inside a runtime directory.
pub fn socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(SOCKET_FILE)
}

/// The engine's metadata file inside a runtime directory, next to the socket.
pub fn meta_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(META_FILE)
}

/// What a running engine publishes about itself in `meta.json` next to its socket,
/// so other processes can list running engines.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DaemonMeta {
    pub project_dir: PathBuf,
    pub pid: u32,
    /// Unix seconds.
    pub started_at: u64,
    pub engine_version: String,
}

impl DaemonMeta {
    /// Reads and parses a `meta.json`; malformed content is an `InvalidData` error.
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Writes the file atomically (owner-only temp file, then rename). The parent
    /// directory must exist.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
        let result = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .and_then(|mut file| {
                // `mode` only applies on creation; cover a leftover temp file.
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                file.write_all(&json)?;
                file.sync_all()
            })
            .and_then(|()| std::fs::rename(&tmp, path));
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }

    /// The current time as unix seconds (0 if the clock is before the epoch).
    pub fn now_unix_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_root_falls_back() {
        assert_eq!(
            runtime_root(Some("/run/user/1"), Some("/t")),
            PathBuf::from("/run/user/1/patok")
        );
        assert_eq!(runtime_root(None, Some("/t")), PathBuf::from("/t/patok"));
        assert_eq!(runtime_root(Some(""), None), PathBuf::from("/tmp/patok"));
        assert_eq!(runtime_root(None, None), PathBuf::from("/tmp/patok"));
        let dir = runtime_dir(Some("/run/user/1"), Some("/t"), Path::new("/p"));
        assert_eq!(
            dir.parent(),
            Some(runtime_root(Some("/run/user/1"), Some("/t")).as_path())
        );
    }

    #[test]
    fn meta_path_sits_next_to_the_socket() {
        let dir = Path::new("/run/patok/x");
        assert_eq!(meta_path(dir).parent(), socket_path(dir).parent());
        assert_eq!(meta_path(dir).file_name().unwrap(), "meta.json");
    }

    fn sample_meta(pid: u32) -> DaemonMeta {
        DaemonMeta {
            project_dir: PathBuf::from("/home/u/proj"),
            pid,
            started_at: 1_700_000_000,
            engine_version: "1.2.3".into(),
        }
    }

    #[test]
    fn daemon_meta_round_trips_through_json_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = meta_path(dir.path());
        let first = sample_meta(1);
        first.write(&path).unwrap();
        assert_eq!(DaemonMeta::read(&path).unwrap(), first);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["meta.json"]);

        let second = sample_meta(2);
        second.write(&path).unwrap();
        assert_eq!(DaemonMeta::read(&path).unwrap(), second);
    }

    #[test]
    fn daemon_meta_read_rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = meta_path(dir.path());
        std::fs::write(&path, "not json").unwrap();
        let error = DaemonMeta::read(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn base_dir_is_the_one_xdg_or_fallback_resolution() {
        const SUB: &str = ".config";
        // The XDG variable set wins, whatever the fallback is.
        assert_eq!(
            base_dir(Some("/xdg"), Some("/home/u"), SUB),
            Some(PathBuf::from("/xdg"))
        );
        assert_eq!(
            base_dir(Some("/xdg"), None, SUB),
            Some(PathBuf::from("/xdg"))
        );
        // Unset: the fallback (HOME) gets the subdir appended.
        assert_eq!(
            base_dir(None, Some("/home/u"), SUB),
            Some(PathBuf::from("/home/u/.config"))
        );
        // Empty counts as unset: the fallback is used, and without one
        // there is no base directory at all.
        assert_eq!(
            base_dir(Some(""), Some("/home/u"), SUB),
            Some(PathBuf::from("/home/u/.config"))
        );
        assert_eq!(base_dir(None, None, SUB), None);
        assert_eq!(base_dir(Some(""), None, SUB), None);
        assert_eq!(base_dir(Some(""), Some(""), SUB), None);
        // An empty subdir uses the fallback as-is (the runtime dir's TMPDIR form).
        assert_eq!(
            base_dir(None, Some("/tmpdir"), ""),
            Some(PathBuf::from("/tmpdir"))
        );
    }

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

    #[test]
    fn config_dir_falls_back_to_home() {
        // The XDG variable set wins, whatever the fallback is.
        assert_eq!(
            config_dir(Some("/xdg"), Some("/home/u")),
            Some(PathBuf::from("/xdg/patok"))
        );
        // Unset or empty XDG falls back to `$HOME/.config`.
        assert_eq!(
            config_dir(None, Some("/home/u")),
            Some(PathBuf::from("/home/u/.config/patok"))
        );
        assert_eq!(
            config_dir(Some(""), Some("/home/u")),
            Some(PathBuf::from("/home/u/.config/patok"))
        );
        // Neither set (or empty) resolves: no config directory.
        assert_eq!(config_dir(None, None), None);
        assert_eq!(config_dir(Some(""), None), None);
        assert_eq!(config_dir(Some(""), Some("")), None);
    }

    #[test]
    fn cache_dir_falls_back_to_home() {
        assert_eq!(
            cache_dir(Some("/xdg"), Some("/home/u")),
            Some(PathBuf::from("/xdg/patok"))
        );
        assert_eq!(
            cache_dir(None, Some("/home/u")),
            Some(PathBuf::from("/home/u/.cache/patok"))
        );
        assert_eq!(
            cache_dir(Some(""), Some("/home/u")),
            Some(PathBuf::from("/home/u/.cache/patok"))
        );
        assert_eq!(cache_dir(None, None), None);
        assert_eq!(cache_dir(Some(""), Some("")), None);
    }
}
