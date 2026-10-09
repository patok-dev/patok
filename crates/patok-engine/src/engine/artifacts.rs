use patok_core::event::NoticeLevel;

use super::Engine;
use crate::artifact;

/// How long a research report on disk stays reusable across restarts.
const RESEARCH_REUSE_SECS: u64 = 600;

/// The build claims prerequisite gate: a file under
/// this many bytes warns and the reviewer falls back to the plan and files.
const CLAIMS_MIN_BYTES: usize = 10;

impl Engine {
    /// Writes the builder's build claims to the project data directory as
    /// `claims/<task id>-<timestamp>.md`, next to
    /// the plans and research folders.
    pub(crate) fn write_claims_file(&self, task_id: &str, claims: &str) {
        let dir = self.inner.config.data_dir.join("claims");
        match artifact::write_artifact(&dir, task_id, claims) {
            Ok(path) => self.notice(
                NoticeLevel::Info,
                format!("Build claims written to {}", path.display()),
            ),
            Err(e) => self.notice(
                NoticeLevel::Warning,
                format!("cannot write {}: {}", e.path.display(), e.error),
            ),
        }
    }

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

    /// The newest research report of `stem` on disk whose filename timestamp
    /// is younger than the reuse window, or `None`.
    /// The stem is a task ID for the per-task research stage and `bootstrap` /
    /// `scan` for the queue-creation runs (T69.1).
    pub(crate) fn reusable_research_report(&self, stem: &str) -> Option<String> {
        let dir = self.inner.config.data_dir.join("research");
        let path = artifact::newest_artifact(&dir, stem, Some(RESEARCH_REUSE_SECS))?;
        std::fs::read_to_string(path).ok()
    }

    /// Writes the research report to the project data directory as
    /// `research/<stem>-<timestamp>.md` (T68.1; the queue-creation runs use
    /// their kind's stem, T69.1).
    pub(crate) fn write_research_report(&self, stem: &str, report: &str) {
        let dir = self.inner.config.data_dir.join("research");
        match artifact::write_artifact(&dir, stem, report) {
            Ok(path) => self.notice(
                NoticeLevel::Info,
                format!("Research report written to {}", path.display()),
            ),
            Err(e) => self.notice(
                NoticeLevel::Warning,
                format!("cannot write {}: {}", e.path.display(), e.error),
            ),
        }
    }

    /// Writes the accepted plan to the project data directory as
    /// `plans/<task id>-<timestamp>.md` (T10.1).
    pub(crate) fn write_plan_file(&self, task_id: &str, plan: &str) {
        let dir = self.inner.config.data_dir.join("plans");
        match artifact::write_artifact(&dir, task_id, plan) {
            Ok(path) => self.notice(
                NoticeLevel::Info,
                format!("Plan written to {}", path.display()),
            ),
            Err(e) => self.notice(
                NoticeLevel::Warning,
                format!("cannot write {}: {}", e.path.display(), e.error),
            ),
        }
    }
}
