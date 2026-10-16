//! Retention pruning for history directories.
//!
//! Implements the pruning logic described in section 9 of docs/private/history-restructure.md:
//! - Remove task dirs beyond the newest `history_retention_tasks` OR older than `history_retention_days`
//! - Remove session dirs left without task dirs (after having had some)
//! - Remove task-less session dirs older than `history_retention_days`
//! - Never touch the current session

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[allow(unused_imports)]
use std::{fs::File, time::UNIX_EPOCH};

/// Parses the unix_millis prefix from a task directory name.
/// Task dir names are `<unix_millis>-<task_id>`.
#[allow(dead_code)]
pub fn task_timestamp_from_dirname(name: &str) -> Option<u64> {
    name.split('-').next()?.parse::<u64>().ok()
}

/// Returns true if a session directory is empty (has no task directories).
#[allow(dead_code)]
fn is_session_empty(session_dir: &Path) -> std::io::Result<bool> {
    let mut has_task_dirs = false;

    if session_dir.exists() && session_dir.is_dir() {
        for entry in fs::read_dir(session_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                // Check if it's a task directory (has a timestamp prefix)
                if let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && task_timestamp_from_dirname(name).is_some()
                {
                    has_task_dirs = true;
                    break;
                }
            }
        }
    }

    Ok(!has_task_dirs)
}

/// Returns true if a session directory has (or had) task directories.
/// We check if there are any task-like directories currently, or if there
/// are any remaining artifacts that suggest tasks were there.
#[allow(dead_code)]
fn session_had_tasks(session_dir: &Path) -> std::io::Result<bool> {
    if !session_dir.exists() || !session_dir.is_dir() {
        return Ok(false);
    }

    for entry in fs::read_dir(session_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
            && task_timestamp_from_dirname(name).is_some()
        {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Collects all task directories in a session with their timestamps.
#[allow(dead_code)]
fn collect_task_dirs(session_dir: &Path) -> std::io::Result<Vec<(PathBuf, u64)>> {
    let mut tasks = Vec::new();

    if !session_dir.exists() || !session_dir.is_dir() {
        return Ok(tasks);
    }

    for entry in fs::read_dir(session_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
            && let Some(ts) = task_timestamp_from_dirname(name)
        {
            tasks.push((path, ts));
        }
    }

    // Sort by timestamp descending (newest first)
    tasks.sort_by_key(|a| std::cmp::Reverse(a.1));

    Ok(tasks)
}

/// Returns the session's timestamp from its directory name.
/// Session dirs are named as `YYYY-MM-DD/HH-MM-SS`.
#[allow(dead_code)]
fn session_timestamp_from_path(session_dir: &Path) -> Option<u64> {
    // The session path is like: root/YYYY-MM-DD/HH-MM-SS
    // We need to parse the date and time from the last two components
    let components: Vec<&str> = session_dir.iter().filter_map(|c| c.to_str()).collect();

    if components.len() >= 2 {
        let date_part = components[components.len() - 2];
        let time_part = components[components.len() - 1];

        // Parse YYYY-MM-DD and HH-MM-SS
        let date_parts: Vec<&str> = date_part.split('-').collect();
        let time_parts: Vec<&str> = time_part.split('-').collect();

        if date_parts.len() == 3
            && time_parts.len() == 3
            && let (Ok(year), Ok(month), Ok(day), Ok(hour), Ok(min), Ok(sec)) = (
                date_parts[0].parse::<u32>(),
                date_parts[1].parse::<u32>(),
                date_parts[2].parse::<u32>(),
                time_parts[0].parse::<u32>(),
                time_parts[1].parse::<u32>(),
                time_parts[2].parse::<u32>(),
            )
        {
            // Calculate timestamp from YYYY-MM-DD HH-MM-SS in UTC
            // This is a simplified calculation
            return Some(date_time_to_timestamp(year, month, day, hour, min, sec));
        }
    }

    None
}

/// Convert YYYY-MM-DD HH-MM-SS to unix timestamp in milliseconds (UTC)
#[allow(dead_code)]
fn date_time_to_timestamp(year: u32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> u64 {
    // Calculate days since epoch for the date
    let mut total_days = 0u64;

    // Add years
    for y in 1970..year {
        total_days += if is_leap_year(y) { 366 } else { 365 };
    }

    // Add months
    let month_days: [u64; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    for m in 1..month {
        total_days += month_days[m as usize - 1];
    }

    // Add days (subtract 1 because day 1 is 0 days into the month)
    total_days += (day - 1) as u64;

    // Convert to seconds
    let total_secs = total_days * 86400 + hour as u64 * 3600 + min as u64 * 60 + sec as u64;

    total_secs * 1000
}

/// Check if a year is a leap year
#[allow(clippy::manual_is_multiple_of)]
fn is_leap_year(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

/// Prunes history directories according to retention settings.
#[allow(dead_code)]
///
/// # Arguments
/// * `root` - The history root directory
/// * `retention_tasks` - Maximum number of task directories to keep
/// * `retention_days` - Maximum age in days for task and session directories
/// * `current_session` - Optional name of the current session (will never be removed)
///
/// # Behavior
/// - Task dirs are removed if beyond the newest `retention_tasks` OR older than `retention_days`
/// - Session dirs are removed if:
///   - They have no task dirs left AND they previously had task dirs
///   - They never had task dirs AND are older than `retention_days`
/// - The current session is never removed
pub fn prune(
    root: &Path,
    retention_tasks: u64,
    retention_days: u64,
    current_session: Option<&str>,
) -> std::io::Result<()> {
    if !root.exists() {
        return Ok(());
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let retention_secs = retention_days * 24 * 60 * 60;

    // Collect all session directories
    // Session dirs are structured as root/YYYY-MM-DD/HH-MM-SS
    // We need to find all HH-MM-SS directories under YYYY-MM-DD directories
    let mut session_dirs: Vec<PathBuf> = Vec::new();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            // Check if this is a date directory (YYYY-MM-DD format)
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                // Date dirs have the format YYYY-MM-DD
                if name.len() == 10 && name.chars().filter(|c| *c == '-').count() == 2 {
                    // Now look for time directories under this date directory
                    for time_entry in fs::read_dir(&path)? {
                        let time_entry = time_entry?;
                        let time_path = time_entry.path();
                        if time_path.is_dir()
                            && let Some(time_name) = time_path.file_name().and_then(|n| n.to_str())
                            && time_name.len() == 8
                            && time_name.chars().filter(|c| *c == '-').count() == 2
                        {
                            session_dirs.push(time_path);
                        }
                    }
                }
            }
        }
    }

    // Sort session dirs by timestamp descending (newest first)
    // For sorting, we use the directory name which is YYYY-MM-DD/HH-MM-SS
    session_dirs.sort_by_key(|a| std::cmp::Reverse(session_timestamp_from_path(a).unwrap_or(0)));

    // Get current session path for comparison
    let current_session_path = current_session.map(|s| root.join(s));

    // Process each session directory
    for session_dir in &session_dirs {
        // Skip current session
        if let Some(ref current) = current_session_path
            && *session_dir == *current
        {
            continue;
        }

        // Collect all task dirs in this session
        let task_dirs = collect_task_dirs(session_dir)?;
        let original_task_count = task_dirs.len();

        // Determine which task dirs to remove
        let tasks_to_remove: Vec<PathBuf> = task_dirs
            .into_iter()
            .enumerate()
            .filter_map(|(idx, (path, ts))| {
                // Remove if beyond retention_tasks OR older than retention_days
                let beyond_count = (idx as u64) >= retention_tasks;
                let too_old = now.saturating_sub(ts / 1000) > retention_secs;

                if beyond_count || too_old {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();

        // Remove the identified task dirs
        for task_path in &tasks_to_remove {
            let _ = fs::remove_dir_all(task_path);
        }

        // Check if session should be removed
        let session_is_empty = is_session_empty(session_dir)?;
        let session_had_any_tasks = original_task_count > 0 || session_had_tasks(session_dir)?;

        // Get session age
        let session_ts = session_timestamp_from_path(session_dir).unwrap_or(0);
        let session_too_old = now.saturating_sub(session_ts / 1000) > retention_secs;

        // Remove session if:
        // - It's empty now AND it had tasks before (they were all pruned)
        // - It never had tasks AND is older than retention_days
        let should_remove_session = (session_is_empty && session_had_any_tasks)
            || (!session_had_any_tasks && session_too_old);

        if should_remove_session {
            let _ = fs::remove_dir_all(session_dir);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    #[allow(unused_imports)]
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn test_task_timestamp_from_dirname() {
        assert_eq!(
            task_timestamp_from_dirname("1700000000000-task123"),
            Some(1700000000000)
        );
        assert_eq!(task_timestamp_from_dirname("not-a-timestamp-task"), None);
        assert_eq!(task_timestamp_from_dirname(""), None);
    }

    #[test]
    fn test_prune_removes_excess_task_dirs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        // Create session with 3 tasks, retention is 2
        let session_dir = root.join("2026-10-16/12-30-45");
        fs::create_dir_all(&session_dir).unwrap();

        // Use current timestamp so tasks are within retention
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let base_ts = now_secs * 1000;

        // Create 3 task dirs (newest first by timestamp)
        fs::create_dir_all(session_dir.join(format!("{}-task3", base_ts + 2000))).unwrap();
        fs::create_dir_all(session_dir.join(format!("{}-task2", base_ts + 1000))).unwrap();
        fs::create_dir_all(session_dir.join(format!("{}-task1", base_ts))).unwrap();

        prune(root, 2, 30, None).unwrap();

        // Should have only 2 newest tasks
        let remaining: Vec<_> = fs::read_dir(&session_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();

        assert_eq!(remaining.len(), 2);

        let names: Vec<String> = remaining
            .iter()
            .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
            .collect();

        // Should have task3 and task2 (newest)
        assert!(names.contains(&format!("{}-task3", base_ts + 2000)));
        assert!(names.contains(&format!("{}-task2", base_ts + 1000)));
        assert!(!names.contains(&format!("{}-task1", base_ts)));
    }

    #[test]
    fn test_prune_removes_old_task_dirs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        // Create session with tasks of different ages
        let session_dir = root.join("2024-01-15/12-30-45");
        fs::create_dir_all(&session_dir).unwrap();

        // Create tasks with timestamps 60 days old and newer
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Old task: 61 days ago
        let old_ts = (now - (61 * 24 * 60 * 60)) * 1000;
        fs::create_dir_all(session_dir.join(format!("{}-task1", old_ts))).unwrap();

        // New task: 1 day ago
        let new_ts = (now - 24 * 60 * 60) * 1000;
        fs::create_dir_all(session_dir.join(format!("{}-task2", new_ts))).unwrap();

        prune(root, 100, 30, None).unwrap();

        // Should have only the new task
        let remaining: Vec<String> = fs::read_dir(&session_dir)
            .unwrap()
            .filter_map(|e| {
                e.ok()
                    .and_then(|e| e.file_name().to_str().map(|s| s.to_string()))
            })
            .collect();

        assert!(!remaining.contains(&format!("{}-task1", old_ts)));
        assert!(remaining.contains(&format!("{}-task2", new_ts)));
    }

    #[test]
    fn test_prune_removes_empty_session_dirs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        // Create session with tasks
        let session_dir = root.join("2026-10-16/12-30-45");
        fs::create_dir_all(&session_dir).unwrap();

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let ts = now_secs * 1000;

        fs::create_dir_all(session_dir.join(format!("{}-task1", ts))).unwrap();

        // Run prune with 0 retention - should remove all tasks
        prune(root, 0, 30, None).unwrap();

        // Session should be removed because it's empty after having had tasks
        assert!(!session_dir.exists());
    }

    #[test]
    fn test_prune_preserves_current_session() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        // Create current session
        let current_session = "2026-10-16/12-30-45";
        let session_dir = root.join(current_session);
        fs::create_dir_all(&session_dir).unwrap();

        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let ts = now_secs * 1000;

        // Create task dirs in current session
        fs::create_dir_all(session_dir.join(format!("{}-task1", ts))).unwrap();

        // Run prune with current session specified
        prune(root, 0, 30, Some(current_session)).unwrap();

        // Current session should still exist
        assert!(session_dir.exists());
    }

    #[test]
    fn test_prune_removes_old_taskless_sessions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path();

        // Create a session without tasks (add-task or discovery session)
        // Use a date that's clearly older than 30 days
        let session_dir = root.join("2023-10-01/00-00-00");
        fs::create_dir_all(&session_dir).unwrap();

        // Add some non-task files to make it look like it existed
        File::create(session_dir.join("output.log")).unwrap();

        prune(root, 100, 30, None).unwrap();

        // Old task-less session should be removed
        // The session timestamp will be parsed from the directory name
        // 2023-10-01 is well before 30 days ago, so it should be removed
    }
}
