use patok_core::event::NoticeLevel;
use patok_core::task::{Task, group_number_of};
use patok_core::utils::atomic_write;

use super::Engine;
use super::group;
use crate::context::SessionContext;
use crate::git::{CommitKind, Git};

impl Engine {
    /// Records the task's batch-review group: a contiguous run of
    /// same-leading-number task IDs in execution order. A changed number
    /// starts a new group: the open-group record decides through
    /// `resume_decision` whether an interrupted group resumes (its base is
    /// restored, so the group's last review still diffs across every commit
    /// of the group) or a fresh one starts at the current HEAD, and the
    /// record is written so the group survives a restart. The group state is
    /// reset at each session start, which makes the decision re-run for the
    /// session's first task.
    pub(crate) async fn note_group(&self, task: &Task, ctx: &SessionContext) {
        let number = task.group_number();
        let current = ctx.group_number();
        if current.is_some() && current == number {
            return;
        }
        let (base, record) = self.group_base(task, number.as_deref()).await;
        if let Some(record) = &record {
            let path = ctx.open_group_path();
            if let Err(error) = record.save(path) {
                tracing::warn!("cannot write the open group {}: {error}", path.display());
            }
        }
        ctx.set_group(number, base);
    }

    /// The base a new batch-review group diffs against, and the open-group
    /// record to write. A group with no leading number (or no commits yet)
    /// records nothing; its base is the current HEAD (`None` without a
    /// repository). Otherwise the record on disk decides: `resume_decision`
    /// resumes an interrupted group (a failed review left its commits behind
    /// with HEAD at the record's `last` commit) by restoring its base, and
    /// anything else -- a moved HEAD, a non-ancestor base, a different
    /// number, or a reused task ID whose description hash differs -- starts
    /// a fresh group whose base and record sit at the current HEAD.
    async fn group_base(
        &self,
        task: &Task,
        number: Option<&str>,
    ) -> (Option<String>, Option<group::OpenGroup>) {
        let git = self.git();
        let head = git.head_full_sha().await;
        let Some(number) = number else {
            return (head, None); // no group: the base diffs HEAD, no record
        };
        let Some(head) = head else {
            return (None, None); // no commits yet: nothing to record
        };
        let fresh = || group::OpenGroup {
            number: number.to_string(),
            base: head.clone(),
            last: head.clone(),
            members: Vec::new(),
        };
        match group::OpenGroup::load(&self.inner.config.open_group) {
            Some(record) => {
                let ancestor = git.is_ancestor(&record.base, &head).await;
                let hash = task.description_hash();
                match group::resume_decision(&record, &head, ancestor, &task.id, &hash) {
                    group::ResumeDecision::Resume => (Some(record.base.clone()), Some(record)),
                    group::ResumeDecision::Fresh => (Some(head.clone()), Some(fresh())),
                }
            }
            None => (Some(head.clone()), Some(fresh())),
        }
    }

    /// Updates the open-group record after an in-group commit: `last` becomes
    /// the new commit's full SHA and the task joins `members`. A missing
    /// record (the group already closed, or no HEAD when it started) is left
    /// alone.
    fn record_group_commit(&self, task: &Task, full: &str) {
        {
            let state = self.state();
            let Some(number) = state.group_number.as_deref() else {
                return;
            };
            if task.group_number().as_deref() != Some(number) {
                return;
            }
        }
        let path = self.inner.config.open_group.clone();
        let Some(mut record) = group::OpenGroup::load(&path) else {
            return;
        };
        record.last = full.to_string();
        let hash = task.description_hash();
        if !record
            .members
            .iter()
            .any(|(id, member_hash)| id == &task.id && member_hash == &hash)
        {
            record.members.push((task.id.clone(), hash));
        }
        if let Err(error) = record.save(&path) {
            tracing::warn!("cannot write the open group {}: {error}", path.display());
        }
    }

    /// Deletes the open-group record when the group's last task completed its
    /// review validated; a failed review keeps it so the rerun still diffs
    /// the whole group. A task outside the current group (or without a
    /// leading number) closes nothing.
    pub(crate) fn close_group(&self, task_id: &str) {
        let number = group_number_of(task_id);
        {
            let state = self.state();
            if number.is_none() || state.group_number != number {
                return;
            }
        }
        let path = self.inner.config.open_group.clone();
        if let Err(error) = group::OpenGroup::clear(&path) {
            tracing::warn!("cannot delete the open group {}: {error}", path.display());
        }
    }

    /// Commit finalization: the five-position
    /// progress token is written into the task line under the task-file lock
    /// as the last mutation before the commit, the task is marked done only
    /// when validated, and the commit is `feat` or `WIP` accordingly. The
    /// builder-failure and review-failure paths finalize the same way with
    /// their own token and `validated` false. No crash-recovery checkpoint is
    /// written for the review stage.
    pub(crate) async fn finalize(
        &self,
        task: &Task,
        ctx: &SessionContext,
        token: &str,
        validated: bool,
    ) -> Option<String> {
        let path = self.inner.config.task_file();
        match self
            .inner
            .task_list
            .write_progress(task, token, validated)
            .await
        {
            Ok(true) => {}
            Ok(false) => self.notice(
                NoticeLevel::Warning,
                format!(
                    "{} was not found as an unchecked task; not ticked.",
                    task.id
                ),
            ),
            Err(e) => self.notice(
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
        let commit = self
            .git()
            .commit_all(kind, &task.description, &[&exclude])
            .await;
        let full = commit.as_ref().map(|commit| commit.full.clone());
        let commit = commit.map(|commit| commit.short);
        if let Some(full) = full {
            self.record_group_commit(task, &full);
        }
        self.report_commit(commit.as_deref());
        commit
    }

    fn report_commit(&self, commit: Option<&str>) {
        match commit {
            Some(sha) => self.notice(NoticeLevel::Info, format!("Committed {sha}")),
            None => self.notice(
                NoticeLevel::Info,
                "Not committed (not a git repository, nothing to commit, or the commit failed).",
            ),
        }
    }

    pub(crate) fn git(&self) -> Git {
        Git::new(&self.inner.config.project_dir)
    }
}
