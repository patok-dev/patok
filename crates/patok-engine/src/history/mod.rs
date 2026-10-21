//! The history module for session and task history management.
//!
//! This module implements the history restructure described in docs/private/history-restructure.md,
//! sections 2-9:
//!
//! - **Root:** resolved from `settings.history_dir`, fallback to `<data_dir>/history`
//! - **Layout:** session dirs `YYYY-MM-DD/HH-MM-SS` in local time, task dirs
//!   `<unix_millis>-<task_id>`, with a per-task zero-padded `<seq>` counter shared across agents
//! - **Writer:** `HistoryWriter` writes `output.log`, `orchestrator.log` and `events.jsonl` to the
//!   open task dir, or to the session dir when no task is open
//! - **Markers:** `SessionOpen` from `Engine::start_*`, `SessionClose` from
//!   `SessionCloseGuard`, `TaskOpen` from `begin_task` and the first-task path, `TaskClose`
//!   after `TaskFinished`
//! - **Files written elsewhere into the same dirs:** raw `-raw.jsonl` and `-result.json` from
//!   `step::agent_run`, Markdown artifacts from `artifact::write_artifact`, batch review output
//!   in the batch's last task dir (`OpenGroup::task_dirs`)
//! - `open-group.json` lives outside the root in the project data dir
//! - **Retention:** `prune` at session open removes task dirs beyond `history_retention_tasks`
//!   OR older than `history_retention_days`, session dirs left empty after pruning, and
//!   task-less session dirs older than `history_retention_days`; never touches the current session
//!
//! The history is write-only: nothing in patok reads it back.

use std::path::{Path, PathBuf};

use patok_config::DaemonSettings;

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
