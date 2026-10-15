//! The per-task commit step: the progress write and the `feat`/`WIP` commit.

use patok_core::event::NoticeLevel;
use patok_core::task::Task;
use patok_core::utils::atomic_write;

use crate::context::SessionContext;
use crate::git::{Commit, CommitKind};

/// The per-task commit step: one task's commit finalization. The
/// five-position progress token is written into the task line through the
/// TaskList as the last mutation before the commit (the task is marked done
/// only when validated), the queue is reconciled, and everything is
/// committed as `feat` or `WIP` accordingly. The builder-failure and
/// review-failure paths finalize the same way with their own token and
/// `validated` false. Returns the commit with its short SHA (display) and
/// full SHA (the open-group record, written by the caller), or `None` when
/// nothing was committed. No crash-recovery checkpoint is written for the
/// commit.
pub(crate) struct CommitStep<'a> {
    ctx: &'a SessionContext,
}

impl<'a> CommitStep<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    pub(crate) async fn run(&self, task: &Task, token: &str, validated: bool) -> Option<Commit> {
        let ctx = self.ctx;
        let path = ctx.task_file().to_path_buf();
        match ctx.write_progress(task, token, validated).await {
            Ok(true) => {}
            Ok(false) => ctx.notice(
                NoticeLevel::Warning,
                format!(
                    "{} was not found as an unchecked task; not ticked.",
                    task.id
                ),
            ),
            Err(e) => ctx.notice(
                NoticeLevel::Error,
                format!("cannot update {}: {e}", path.display()),
            ),
        }
        ctx.reconcile();
        let kind = if validated {
            CommitKind::Feat
        } else {
            CommitKind::Wip
        };
        // The task file's temp-file glob stays out of the commit even when
        // the startup sweep missed an orphan (a temp created after it ran).
        let exclude = atomic_write::temp_glob(&path);
        let commit = ctx
            .git()
            .commit_all(kind, &task.description, &[&exclude])
            .await;
        match &commit {
            Some(commit) => ctx.notice(NoticeLevel::Info, format!("Committed {}", commit.short)),
            None => ctx.notice(
                NoticeLevel::Info,
                "Not committed (not a git repository, nothing to commit, or the commit failed).",
            ),
        }
        commit
    }
}
