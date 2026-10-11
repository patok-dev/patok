use patok_core::event::NoticeLevel;

use super::Engine;
use crate::artifact;

/// The build claims prerequisite gate: a file under
/// this many bytes warns and the reviewer falls back to the plan and files.
const CLAIMS_MIN_BYTES: usize = 10;

impl Engine {
    /// The newest build claims of `task_id` on disk, when it exists and passes
    /// the prerequisite gate of at least 10 bytes;
    /// the gate is a warning only, and the reviewer prompt falls back to the
    /// plan and the changed-file list.
    pub(crate) fn claims_file(&self, task_id: &str) -> Option<String> {
        let dir = self.inner.config.data_dir.join("claims");
        let newest = artifact::newest_artifact(&dir, task_id, None);
        let found = newest.and_then(|path| {
            std::fs::read_to_string(&path)
                .ok()
                .map(|claims| (path, claims))
        });
        let Some((_path, claims)) = found else {
            // The gate is a warning only: the reviewer proceeds with the plan
            // and the changed files as the fallback.
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "no build claims found for {task_id}; the reviewer falls back to the plan and the changed files"
                ),
            );
            return None;
        };
        if claims.len() < CLAIMS_MIN_BYTES {
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "the build claims for {task_id} are under {CLAIMS_MIN_BYTES} bytes; the reviewer falls back to the plan and the changed files"
                ),
            );
            return None;
        }
        Some(claims)
    }

    /// Writes the review report to the project data directory as
    /// `reviews/<task id>-<timestamp>.md`, returning
    /// its path for the failure notice.
    pub(crate) fn write_review_file(
        &self,
        task_id: &str,
        report: &str,
    ) -> Option<std::path::PathBuf> {
        let dir = self.inner.config.data_dir.join("reviews");
        match artifact::write_artifact(&dir, task_id, report) {
            Ok(path) => {
                self.notice(
                    NoticeLevel::Info,
                    format!("Review report written to {}", path.display()),
                );
                Some(path)
            }
            Err(e) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!("cannot write {}: {}", e.path.display(), e.error),
                );
                None
            }
        }
    }
}
