//! Directory layout for history: session and task directories.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[allow(unused_imports)]
use std::time::UNIX_EPOCH;

/// The root directory for history storage.
#[derive(Debug, Clone)]
pub struct HistoryRoot {
    path: PathBuf,
}

impl HistoryRoot {
    /// Creates a new HistoryRoot from a path.
    pub fn new<P: Into<PathBuf>>(path: P) -> Self {
        Self { path: path.into() }
    }

    /// Returns the root path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Ensures the root directory exists.
    pub fn ensure(&self) -> std::io::Result<()> {
        if !self.path.exists() {
            fs::create_dir_all(&self.path)?;
        }
        Ok(())
    }
}

/// Generates the session directory name in UTC as `YYYY-MM-DD/HH-MM-SS`.
/// We use UTC to avoid adding a chrono dependency. Local time can be added later
/// if chrono is added to the workspace.
pub fn session_dir_name() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();

    let secs = now.as_secs();
    let days_since_epoch = secs / 86400;
    let secs_today = secs % 86400;

    // Calculate year, month, day from days since epoch (1970-01-01)
    // This is a simplified calculation that works for dates after 1970
    let (year, month, day) = days_to_ymd(days_since_epoch as u32);
    let (hour, min, sec) = secs_to_hms(secs_today as u32);

    format!(
        "{:04}-{:02}-{:02}/{:02}-{:02}-{:02}",
        year, month, day, hour, min, sec
    )
}

/// Convert days since epoch (1970-01-01) to year, month, day
#[allow(dead_code)]
fn days_to_ymd(days: u32) -> (u32, u32, u32) {
    // Simple algorithm for dates after 1970
    let mut remaining_days = days;
    let mut year = 1970u32;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    let month_days: [u32; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1u32;
    for days_in_month in month_days.iter() {
        if remaining_days < *days_in_month {
            break;
        }
        remaining_days -= *days_in_month;
        month += 1;
    }

    let day = remaining_days + 1;

    (year, month, day)
}

/// Check if a year is a leap year
#[allow(dead_code)]
#[allow(clippy::manual_is_multiple_of)]
fn is_leap_year(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Convert seconds of day to hour, minute, second
#[allow(dead_code)]
fn secs_to_hms(secs: u32) -> (u32, u32, u32) {
    let hour = secs / 3600;
    let remaining = secs % 3600;
    let min = remaining / 60;
    let sec = remaining % 60;
    (hour, min, sec)
}

/// Generates the task directory name as `<unix_millis>-<task_id>`.
/// The timestamp is the unix timestamp in milliseconds.
pub fn task_dir_name(task_id: &str, timestamp: u64) -> String {
    format!("{}-{}", timestamp, task_id)
}

/// Returns the next zero-padded 3-digit sequence counter for a task.
/// The counter is shared across agents and counts every agent run.
/// It is computed from the existing files in the task directory.
#[allow(dead_code)]
pub fn sequence_counter(task_dir: &Path) -> std::io::Result<u32> {
    let mut max_seq = 0u32;

    if task_dir.exists() {
        for entry in fs::read_dir(task_dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            // Parse the sequence from filenames like "001-agent-provider-raw.jsonl"
            // or "001-agent-result.json"
            if let Some(prefix) = name.split('-').next()
                && let Ok(seq) = prefix.parse::<u32>()
            {
                max_seq = max_seq.max(seq);
            }
        }
    }

    Ok(max_seq + 1)
}

/// Resolves the full path to a session directory.
#[allow(dead_code)]
pub fn resolve_session_dir(root: &Path, session_name: &str) -> PathBuf {
    root.join(session_name)
}

/// Resolves the full path to a task directory within a session.
#[allow(dead_code)]
pub fn resolve_task_dir(session_dir: &Path, task_name: &str) -> PathBuf {
    session_dir.join(task_name)
}

/// Ensures a session directory exists and returns its path.
pub fn ensure_session_dir(root: &Path, session_name: &str) -> std::io::Result<PathBuf> {
    let path = resolve_session_dir(root, session_name);
    fs::create_dir_all(&path)?;
    Ok(path)
}

/// Ensures a task directory exists within a session directory and returns its path.
pub fn ensure_task_dir(session_dir: &Path, task_name: &str) -> std::io::Result<PathBuf> {
    let path = resolve_task_dir(session_dir, task_name);
    fs::create_dir_all(&path)?;
    Ok(path)
}

/// Returns the current system time as unix timestamp in milliseconds.
#[allow(dead_code)]
pub fn current_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_dir_name_format() {
        let name = session_dir_name();
        // Should be in format YYYY-MM-DD/HH-MM-SS
        // YYYY-MM-DD has 2 hyphens, HH-MM-SS has 2 hyphens = 4 total
        assert_eq!(name.chars().filter(|c| *c == '-').count(), 4);
        assert_eq!(name.chars().filter(|c| *c == '/').count(), 1);
        let parts: Vec<&str> = name.split('/').collect();
        assert_eq!(parts.len(), 2);
        // Date part: YYYY-MM-DD
        assert_eq!(parts[0].chars().count(), 10);
        assert_eq!(parts[0].chars().filter(|c| *c == '-').count(), 2);
        // Time part: HH-MM-SS
        assert_eq!(parts[1].chars().count(), 8);
        assert_eq!(parts[1].chars().filter(|c| *c == '-').count(), 2);
    }

    #[test]
    fn test_task_dir_name_format() {
        let name = task_dir_name("task123", 1700000000000);
        assert_eq!(name, "1700000000000-task123");
    }

    #[test]
    fn test_sequence_counter_empty_dir() {
        let temp_dir = tempfile::tempdir().unwrap();
        let counter = sequence_counter(temp_dir.path()).unwrap();
        assert_eq!(counter, 1);
    }

    #[test]
    fn test_sequence_counter_with_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path();

        // Create some sequence files
        fs::File::create(path.join("001-agent-provider-raw.jsonl")).unwrap();
        fs::File::create(path.join("002-agent-result.json")).unwrap();
        fs::File::create(path.join("005-other-file.txt")).unwrap();

        let counter = sequence_counter(path).unwrap();
        assert_eq!(counter, 6);
    }

    #[test]
    fn test_resolve_paths() {
        let root = Path::new("/tmp/history");
        let session = "2024-01-15/12-30-45";
        let task = "1700000000000-task123";

        let session_path = resolve_session_dir(root, session);
        assert_eq!(session_path, Path::new("/tmp/history/2024-01-15/12-30-45"));

        let task_path = resolve_task_dir(&session_path, task);
        assert_eq!(
            task_path,
            Path::new("/tmp/history/2024-01-15/12-30-45/1700000000000-task123")
        );
    }
}
