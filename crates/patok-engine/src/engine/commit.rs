use patok_core::task::{Task, group_number_of};

use super::Engine;
use super::group;
use crate::context::SessionContext;
use crate::git::Git;

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
    pub(crate) fn record_group_commit(&self, task: &Task, full: &str) {
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

    pub(crate) fn git(&self) -> Git {
        Git::new(&self.inner.config.project_dir)
    }
}
