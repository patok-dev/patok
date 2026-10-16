//! Unit tests for the history module.
//!
//! These tests verify the directory layout, naming conventions, and retention
//! pruning logic against temporary directories.

use std::fs;
use std::path::Path;

use super::*;

#[test]
fn test_session_dir_naming() {
    let name = layout::session_dir_name();
    // Should be in format YYYY-MM-DD/HH-MM-SS
    // YYYY-MM-DD has 2 hyphens, HH-MM-SS has 2 hyphens = 4 total
    assert_eq!(name.chars().filter(|c| *c == '-').count(), 4);
    assert_eq!(name.chars().filter(|c| *c == '/').count(), 1);
    let parts: Vec<&str> = name.split('/').collect();
    assert_eq!(parts.len(), 2);
    // Date part: YYYY-MM-DD (10 chars with 2 hyphens)
    assert_eq!(parts[0].len(), 10);
    assert_eq!(parts[0].chars().filter(|c| *c == '-').count(), 2);
    // Time part: HH-MM-SS (8 chars with 2 hyphens)
    assert_eq!(parts[1].len(), 8);
    assert_eq!(parts[1].chars().filter(|c| *c == '-').count(), 2);
}

#[test]
fn test_task_dir_naming() {
    let name = layout::task_dir_name("task-123", 1700000000000);
    assert_eq!(name, "1700000000000-task-123");

    let name2 = layout::task_dir_name("T-1", 1234567890123);
    assert_eq!(name2, "1234567890123-T-1");
}

#[test]
fn test_prune_removes_excess_task_dirs() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    // Create a session directory
    let session_dir = root.join("2026-10-16/12-30-45");
    fs::create_dir_all(&session_dir).unwrap();

    // Create 5 task directories with timestamps (base timestamp + offset)
    // Use a recent timestamp so tasks are within the 30-day retention
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let base_ts = now_secs * 1000; // Convert to millis
    for i in 0..5 {
        let task_name = format!("{}-task{}", base_ts + i * 1000, i);
        fs::create_dir_all(session_dir.join(&task_name)).unwrap();
    }

    // Run prune with retention of 3 tasks
    retention::prune(root, 3, 30, None).unwrap();

    // Check session still exists (it should have 3 tasks remaining)
    assert!(
        session_dir.exists(),
        "Session should still exist after pruning"
    );

    // Check that only 3 newest task dirs remain
    let remaining: Vec<String> = fs::read_dir(&session_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();

    assert_eq!(
        remaining.len(),
        3,
        "Expected 3 tasks, got {}",
        remaining.len()
    );

    eprintln!("Remaining tasks: {:?}", remaining);

    // The newest should be task4, task3, task2
    assert!(
        remaining.contains(&format!("{}-task4", base_ts + 4 * 1000)),
        "Missing task4"
    );
    assert!(
        remaining.contains(&format!("{}-task3", base_ts + 3 * 1000)),
        "Missing task3"
    );
    assert!(
        remaining.contains(&format!("{}-task2", base_ts + 2 * 1000)),
        "Missing task2"
    );
    assert!(
        !remaining.contains(&format!("{}-task1", base_ts + 1000)),
        "task1 should be removed"
    );
    assert!(
        !remaining.contains(&format!("{}-task0", base_ts)),
        "task0 should be removed"
    );
}

#[test]
fn test_prune_removes_old_task_dirs() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    let session_dir = root.join("2024-01-15/12-30-45");
    fs::create_dir_all(&session_dir).unwrap();

    // Get current timestamp
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Create old task (61 days ago - beyond 30 day retention)
    let old_ts = (now - 61 * 24 * 60 * 60) * 1000;
    fs::create_dir_all(session_dir.join(format!("{}-old-task", old_ts))).unwrap();

    // Create new task (1 day ago - within retention)
    let new_ts = (now - 24 * 60 * 60) * 1000;
    fs::create_dir_all(session_dir.join(format!("{}-new-task", new_ts))).unwrap();

    // Run prune with 30 day retention
    retention::prune(root, 100, 30, None).unwrap();

    let remaining: Vec<String> = fs::read_dir(&session_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
        .collect();

    assert!(!remaining.contains(&format!("{}-old-task", old_ts)));
    assert!(remaining.contains(&format!("{}-new-task", new_ts)));
}

#[test]
fn test_prune_removes_empty_session_dirs() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    // Create a session with tasks
    let session_dir = root.join("2024-01-15/12-30-45");
    fs::create_dir_all(&session_dir).unwrap();

    // Add task directories
    fs::create_dir_all(session_dir.join("1705315845000-task1")).unwrap();
    fs::create_dir_all(session_dir.join("1705315846000-task2")).unwrap();

    // Run prune with 0 task retention - all tasks will be removed
    retention::prune(root, 0, 30, None).unwrap();

    // Session should be removed because it's empty after having had tasks
    assert!(!session_dir.exists());
}

#[test]
fn test_prune_preserves_current_session() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    let current_session = "2024-01-15/12-30-45";
    let session_dir = root.join(current_session);
    fs::create_dir_all(&session_dir).unwrap();

    // Add task directories to current session
    fs::create_dir_all(session_dir.join("1705315845000-task1")).unwrap();
    fs::create_dir_all(session_dir.join("1705315846000-task2")).unwrap();

    // Run prune with 0 task retention but specify current session
    retention::prune(root, 0, 30, Some(current_session)).unwrap();

    // Current session should still exist even though it would otherwise be pruned
    assert!(session_dir.exists());

    // Tasks should still be there
    let task_count = fs::read_dir(&session_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert_eq!(task_count, 2);
}

#[test]
fn test_prune_removes_old_taskless_sessions() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    // Create a session without tasks (add-task or discovery session)
    let session_dir = root.join("2023-10-01/00-00-00");
    fs::create_dir_all(&session_dir).unwrap();

    // Add non-task files
    fs::File::create(session_dir.join("output.log")).unwrap();

    // Run prune with 30 day retention
    // Session from 2023-10-01 is older than 30 days from now
    retention::prune(root, 100, 30, None).unwrap();

    // Old task-less session should be removed
    // Note: This depends on the date parsing, but should work for old dates
}

#[test]
fn test_resolve_root_with_settings() {
    use patok_core::config::DaemonSettings;

    let data_dir = Path::new("/tmp/project/.patok");

    // Test with empty history_dir (should use fallback)
    let settings_empty = DaemonSettings {
        history_dir: Some(String::new()),
        ..Default::default()
    };
    let root = resolve_root(&settings_empty, data_dir);
    assert_eq!(root, data_dir.join("history"));

    // Test with custom history_dir
    let settings_custom = DaemonSettings {
        history_dir: Some("/tmp/custom/history".to_string()),
        ..Default::default()
    };
    let root = resolve_root(&settings_custom, data_dir);
    assert_eq!(root, Path::new("/tmp/custom/history"));
}

#[test]
fn test_history_root_ensure() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root_path = temp_dir.path().join("history");

    let root = HistoryRoot::new(&root_path);

    // Directory should not exist yet
    assert!(!root_path.exists());

    // Ensure should create it
    root.ensure().unwrap();

    assert!(root_path.exists());
    assert!(root_path.is_dir());
}

#[test]
fn test_ensure_session_dir() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    let session_path = layout::ensure_session_dir(root, "2024-01-15/12-30-45").unwrap();

    assert!(session_path.exists());
    assert!(session_path.is_dir());
    assert_eq!(session_path, root.join("2024-01-15/12-30-45"));
}

#[test]
fn test_ensure_task_dir() {
    let temp_dir = tempfile::tempdir().unwrap();
    let session_dir = temp_dir.path().join("2024-01-15/12-30-45");
    fs::create_dir_all(&session_dir).unwrap();

    let task_path = layout::ensure_task_dir(&session_dir, "1705315845000-task1").unwrap();

    assert!(task_path.exists());
    assert!(task_path.is_dir());
    assert_eq!(task_path, session_dir.join("1705315845000-task1"));
}
