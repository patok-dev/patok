//! The history module for session and task history management.
//!
//! This module implements the directory layout and retention pruning described in
//! docs/private/history-restructure.md, section 2 and 9:
//!
//! - Root: resolved from `settings.history_dir`, fallback to `<data_dir>/history`
//! - Session dirs: local-time `YYYY-MM-DD/HH-MM-SS`
//! - Task dirs: `<unix_millis>-<task_id>`
//! - Per-task zero-padded sequence counter shared across agents
//!
//! Retention pruning removes:
//! - Task dirs beyond the newest `history_retention_tasks` OR older than `history_retention_days`
//! - Session dirs left without task dirs (after having had some)
//! - Task-less session dirs older than `history_retention_days`
//! - Never touches the current session

use std::path::{Path, PathBuf};

use patok_core::config::DaemonSettings;

#[allow(unused_imports)]
pub use layout::HistoryRoot;
pub use layout::{
    ensure_session_dir, ensure_task_dir, sequence_counter, session_dir_name, task_dir_name,
};
#[allow(unused_imports)]
pub use retention::prune;
#[cfg(test)]
#[allow(unused_imports)]
pub use tests::*;

/// The root directory for history, resolved from settings.
pub fn resolve_root(settings: &DaemonSettings, data_dir: &Path) -> PathBuf {
    if let Some(ref dir) = settings.history_dir {
        if dir.is_empty() {
            data_dir.join("history")
        } else {
            PathBuf::from(dir)
        }
    } else {
        data_dir.join("history")
    }
}

mod layout;
mod retention;
mod writer;

#[cfg(test)]
mod tests;

pub use writer::{HistoryMsg, HistoryWriter};
