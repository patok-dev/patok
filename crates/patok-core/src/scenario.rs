//! The startup scenarios: the
//! project-state classification that drives the Explore tab's idle strip and the
//! "What do you want to do?" modal's primary action.
//!
//! The filesystem scan is pure and best-effort: unreadable directories simply do not
//! count. The classification itself is split out ([`classify`]) so the shell can
//! re-derive the scenario live from the engine-reported task counts.

use std::path::Path;

/// The task-queue file. The engine owns its own constant;
/// this one is for the shell-side scan.
pub const TASK_FILE: &str = "TASKS.md";
/// The project spec file the brief is saved into.
pub const SPEC_FILE: &str = "SPEC.md";

/// Directories the scan never enters.
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    "build",
    "vendor",
    "dist",
    "out",
    "venv",
    ".venv",
    "__pycache__",
    ".cache",
    ".npm",
    ".tox",
];

/// Build manifests that count as meaningful project files on their own.
const MANIFESTS: &[&str] = &[
    "cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "pom.xml",
    "build.gradle",
    "gemfile",
    "composer.json",
    "mix.exs",
    "deno.json",
];

/// Common source extensions that make a file meaningful.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "kt", "c", "h", "cpp", "hpp", "cs", "rb",
    "swift", "m", "mm", "scala", "sh", "ex", "exs", "zig", "php", "sql", "html", "css", "vue",
    "svelte",
];

/// One of the four startup scenarios.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    /// No meaningful project files and no task queue file: nothing to continue from.
    EmptyProject,
    /// Code exists but the task file is missing, unparseable or empty.
    NeedsQueue,
    /// The task file has at least one pending task.
    QueueReady,
    /// The task file has tasks but none pending.
    QueueComplete,
}

impl Scenario {
    /// The idle strip's headline line.
    pub fn headline(self) -> &'static str {
        match self {
            Scenario::EmptyProject => "Start a new project",
            Scenario::NeedsQueue => "Code found, but no task queue exists yet.",
            Scenario::QueueReady => "Work is ready to continue.",
            Scenario::QueueComplete => "Current task queue is complete.",
        }
    }

    /// The idle strip's detail line under the headline.
    pub fn detail(self) -> &'static str {
        match self {
            Scenario::EmptyProject => "Describe what you want to build.",
            Scenario::NeedsQueue => "Press Enter to scan the project into a task queue.",
            Scenario::QueueReady => "Press s to start the build loop.",
            Scenario::QueueComplete => "Press Enter to scan for follow-up work.",
        }
    }
}

/// The task-queue file's state on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskFileState {
    /// No file.
    #[default]
    Missing,
    /// The file exists but holds only whitespace.
    Empty,
    /// The file has content but no parseable task line.
    Invalid,
    /// The file parses to at least one task.
    Ok,
}

/// The spec file's state on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpecState {
    /// No file.
    #[default]
    Missing,
    /// The file exists but has no non-heading content.
    Empty,
    /// At least one non-blank line does not start with `#`.
    Content,
}

/// What the project scan found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectScan {
    /// Any meaningful project file exists within depth 3.
    pub has_code: bool,
    /// The task-queue file's state.
    pub task_file: TaskFileState,
    /// The spec file's state.
    pub spec: SpecState,
}

/// Scans the project's files within depth 3 (a file three directories down counts,
/// one level deeper does not), skipping the usual non-project directories, and reads
/// the task-queue and spec file states.
pub fn scan_project(dir: &Path) -> ProjectScan {
    let mut scan = ProjectScan::default();
    walk(dir, 0, &mut scan);
    scan.task_file = task_file_state(&dir.join(TASK_FILE));
    scan.spec = spec_state(&dir.join(SPEC_FILE));
    scan
}

/// Visits every file in `dir` (itself `depth` levels below the project root), marking
/// the first meaningful file and recursing while the depth-3 boundary allows.
fn walk(dir: &Path, depth: usize, scan: &mut ProjectScan) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_dir() {
            if depth < 3 && !SKIPPED_DIRS.contains(&name.as_ref()) {
                walk(&entry.path(), depth + 1, scan);
            }
        } else if !scan.has_code && is_meaningful(&name) {
            scan.has_code = true;
        }
    }
}

/// Whether a file counts as a meaningful project file: spec/task/architecture files,
/// build manifests, Dockerfile/Makefile and common source extensions count; READMEs,
/// agent notes, ignore files and OS clutter do not.
fn is_meaningful(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("readme")
        || lower.contains("agent-notes")
        || matches!(lower.as_str(), "claude.md" | "agents.md" | ".ds_store")
        || lower.ends_with("ignore")
    {
        return false;
    }
    if lower.contains("spec") || lower.contains("architecture") {
        return true;
    }
    if MANIFESTS.contains(&lower.as_str()) || matches!(lower.as_str(), "dockerfile" | "makefile") {
        return true;
    }
    lower
        .rsplit('.')
        .next()
        .is_some_and(|extension| SOURCE_EXTENSIONS.contains(&extension))
}

fn task_file_state(path: &Path) -> TaskFileState {
    let Ok(text) = std::fs::read_to_string(path) else {
        return TaskFileState::Missing;
    };
    if text.trim().is_empty() {
        return TaskFileState::Empty;
    }
    if crate::task::parse(&text).is_empty() {
        return TaskFileState::Invalid;
    }
    TaskFileState::Ok
}

fn spec_state(path: &Path) -> SpecState {
    let Ok(text) = std::fs::read_to_string(path) else {
        return SpecState::Missing;
    };
    let has_content = text
        .lines()
        .any(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'));
    if has_content {
        SpecState::Content
    } else {
        SpecState::Empty
    }
}

/// Classifies the project into one of the four scenarios from the scan's file facts
/// and the live task counts (which the shell re-reads on every queue change).
pub fn classify(
    has_code: bool,
    task_file: TaskFileState,
    pending: usize,
    total: usize,
) -> Scenario {
    if pending > 0 {
        return Scenario::QueueReady;
    }
    if total > 0 {
        return Scenario::QueueComplete;
    }
    // Code exists, or a task file does (empty or unparseable): the queue is what
    // is missing either way.
    if has_code || task_file != TaskFileState::Missing {
        Scenario::NeedsQueue
    } else {
        Scenario::EmptyProject
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().to_path_buf();
        (temp, path)
    }

    fn write(path: &std::path::Path, name: &str, contents: &str) {
        fs::write(path.join(name), contents).unwrap();
    }

    #[test]
    fn classifies_all_four_scenarios() {
        use Scenario::*;
        assert_eq!(classify(false, TaskFileState::Missing, 0, 0), EmptyProject);
        assert_eq!(classify(true, TaskFileState::Missing, 0, 0), NeedsQueue);
        assert_eq!(classify(false, TaskFileState::Empty, 0, 0), NeedsQueue);
        assert_eq!(classify(false, TaskFileState::Invalid, 0, 0), NeedsQueue);
        // Pending tasks win over the file facts.
        assert_eq!(classify(false, TaskFileState::Missing, 1, 3), QueueReady);
        // Tasks with none pending complete the queue.
        assert_eq!(classify(true, TaskFileState::Ok, 0, 2), QueueComplete);
    }

    #[test]
    fn an_empty_directory_is_an_empty_project() {
        let (_temp, path) = dir();
        // READMEs, agent notes, ignore files and OS clutter do not count.
        write(&path, "README.md", "# empty\n");
        write(&path, "AGENTS.md", "notes\n");
        write(&path, ".gitignore", "target\n");
        write(&path, ".DS_Store", "junk");
        let scan = scan_project(&path);
        assert!(!scan.has_code);
        assert_eq!(scan.task_file, TaskFileState::Missing);
        assert_eq!(scan.spec, SpecState::Missing);
        assert_eq!(
            classify(scan.has_code, scan.task_file, 0, 0),
            Scenario::EmptyProject
        );
    }

    #[test]
    fn a_single_source_file_flips_to_needs_queue() {
        let (_temp, path) = dir();
        write(&path, "main.rs", "fn main() {}\n");
        let scan = scan_project(&path);
        assert!(scan.has_code);
        assert_eq!(
            classify(scan.has_code, scan.task_file, 0, 0),
            Scenario::NeedsQueue
        );
    }

    #[test]
    fn every_skipped_directory_is_skipped() {
        for name in SKIPPED_DIRS {
            let (_temp, path) = dir();
            fs::create_dir_all(path.join(name).join("nested")).unwrap();
            write(&path.join(name), "main.rs", "fn main() {}\n");
            write(&path.join(name).join("nested"), "main.rs", "fn main() {}\n");
            assert!(!scan_project(&path).has_code, "skipped {name}");
        }
    }

    #[test]
    fn the_depth_three_boundary_counts_three_dirs_but_not_four() {
        let (_temp, path) = dir();
        fs::create_dir_all(path.join("a/b/c/d")).unwrap();
        write(&path.join("a/b/c"), "lib.rs", "pub fn f() {}\n");
        assert!(scan_project(&path).has_code);
        let (_temp, path) = dir();
        fs::create_dir_all(path.join("a/b/c/d")).unwrap();
        write(&path.join("a/b/c/d"), "lib.rs", "pub fn f() {}\n");
        assert!(!scan_project(&path).has_code);
    }

    #[test]
    fn manifests_dockerfile_makefile_and_spec_files_count() {
        for name in [
            "Cargo.toml",
            "package.json",
            "go.mod",
            "Dockerfile",
            "Makefile",
            "my-spec.md",
            "architecture.md",
        ] {
            let (_temp, path) = dir();
            write(&path, name, "content\n");
            assert!(scan_project(&path).has_code, "{name} counts");
        }
    }

    #[test]
    fn task_file_states_are_read_from_disk() {
        let (_temp, path) = dir();
        assert_eq!(scan_project(&path).task_file, TaskFileState::Missing);
        write(&path, TASK_FILE, "   \n\t\n");
        assert_eq!(scan_project(&path).task_file, TaskFileState::Empty);
        write(&path, TASK_FILE, "# Tasks\nOnly prose, no task lines.\n");
        assert_eq!(scan_project(&path).task_file, TaskFileState::Invalid);
        write(&path, TASK_FILE, "- [ ] T1.1: first\n");
        assert_eq!(scan_project(&path).task_file, TaskFileState::Ok);
    }

    #[test]
    fn spec_states_are_read_from_disk() {
        let (_temp, path) = dir();
        assert_eq!(scan_project(&path).spec, SpecState::Missing);
        write(&path, SPEC_FILE, "# Brief\n## Details\n");
        assert_eq!(scan_project(&path).spec, SpecState::Empty);
        write(&path, SPEC_FILE, "# Brief\nA web service for recipes.\n");
        assert_eq!(scan_project(&path).spec, SpecState::Content);
    }
}
