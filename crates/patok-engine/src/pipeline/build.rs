//! The per-task build pipeline and its batch-review group wiring.

use patok_core::event::TaskOutcome;
use patok_core::pipeline::{Stage, TileStatus};
use patok_core::task::Task;

use crate::context::SessionContext;
use crate::pipeline::group;
use crate::step::build::{BuildOutcome, BuildStep};
use crate::step::commit::CommitStep;
use crate::step::plan::{PlanOutcome, PlanStep};
use crate::step::research::{ResearchOutcome, ResearchStep};
use crate::step::review::{ReviewOutcome, ReviewStep};

/// One task's build pipeline (section 10 of docs/private/engine-restructure.md):
/// it configures and orders the research, plan, build, review and commit
/// steps of the task's build session, passes each step's output to the next,
/// owns every tile update of the flow and computes the five-position progress
/// token. Created per task over the session context; the batch-review group
/// wiring (noting, recording, closing) lives here as private methods, with
/// its cross-task state in the engine state through the context.
pub(crate) struct BuildPipeline<'a> {
    ctx: &'a SessionContext,
}

impl<'a> BuildPipeline<'a> {
    pub(crate) fn new(ctx: &'a SessionContext) -> Self {
        Self { ctx }
    }

    /// Runs one task's build session, preceded by the research step (T68.1)
    /// and the plan step when it is enabled (`plan_enabled`, T10.1). Returns
    /// the task outcome and the commit's short SHA; the session-level loop
    /// ends the task with them. All steps run on the session's settings
    /// snapshot in the context, and every tile update of the four steps
    /// happens here (the review tile is active on entering the review step,
    /// section 11): the steps themselves never touch tiles.
    pub(crate) async fn run(&self, task: &Task) -> (TaskOutcome, Option<String>) {
        // The batch-review group of this task is noted first: a new leading
        // number starts or resumes a group through the open-group record,
        // capturing the base its last review diffs across.
        self.note_group(task).await;

        // The research step (T68.1): one research session runs before the task is
        // planned (or built, with the plan step off), unless a fresh report is on
        // disk to reuse or the task is Simple with the skip key on. A failed session
        // is non-blocking: the pipeline continues without a report. The research
        // tile is active only when a session runs, so a reused or skipped report
        // never flips it.
        let research_step = ResearchStep::new(self.ctx);
        if research_step.runs_session(task) {
            self.ctx
                .update_tiles(|tiles| tiles.set(Stage::Research, TileStatus::Active));
        }
        let mut ran_research = true;
        let research = match research_step.run(task).await {
            ResearchOutcome::Cancelled => {
                return (TaskOutcome::Cancelled, None);
            }
            ResearchOutcome::Report(text) => Some(text),
            ResearchOutcome::Skipped => {
                ran_research = false;
                None
            }
            ResearchOutcome::Failed => None,
        };

        // The plan step: behind the `plan_enabled` key, one plan session with the
        // read-only allowlist runs before the builder session. A plan rejected twice
        // fails the task without a builder session, a commit or a task-file tick.
        // The research report is the plan prompt's prior artifact (T68.1).
        let plan = if self.ctx.settings().plan_enabled {
            self.ctx.update_tiles(|tiles| {
                tiles.set(Stage::Research, TileStatus::Done);
                tiles.set(Stage::Plan, TileStatus::Active);
            });
            match PlanStep::new(self.ctx).run(task, research.as_deref()).await {
                PlanOutcome::Accepted(text) => Some(text),
                PlanOutcome::Cancelled => {
                    return (TaskOutcome::Cancelled, None);
                }
                PlanOutcome::Failed => {
                    return (TaskOutcome::Failed, None);
                }
            }
        } else {
            None
        };
        let progress = |ran_review: bool, validated: bool| {
            group::progress(ran_research, plan.is_some(), true, ran_review, validated)
        };

        // The builder takes over from the planner (T11.1); also announced when the plan
        // step is disabled, so the shell never shows a stale agent. The research and
        // plan tiles are done and the build tile active from here on -- also when a
        // step was skipped, so the stages before the running one are always done.
        self.ctx.update_tiles(|tiles| {
            tiles.set(Stage::Research, TileStatus::Done);
            tiles.set(Stage::Plan, TileStatus::Done);
            tiles.set(Stage::Build, TileStatus::Active);
        });

        match BuildStep::new(self.ctx)
            .run(task, plan.as_deref(), research.as_deref())
            .await
        {
            BuildOutcome::Failed => {
                let commit = self.commit(task, &progress(false, false), false).await;
                (TaskOutcome::Failed, commit)
            }
            BuildOutcome::Completed { claims } => {
                // The build tile is done once the builder session completed: the
                // stages before the running one are always done, so review never renders with the build
                // still in-progress.
                self.ctx
                    .update_tiles(|tiles| tiles.set(Stage::Build, TileStatus::Done));
                // The review tile is active from entering the review step,
                // before the skip decision and the diff (section 11).
                self.ctx
                    .update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Active));
                let base = self.ctx.group_base();
                let review = ReviewStep::new(self.ctx)
                    .run(task, plan.as_deref(), claims.as_deref(), base.as_deref())
                    .await;
                self.ctx
                    .update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Done));
                match review {
                    ReviewOutcome::Cancelled => (TaskOutcome::Cancelled, None),
                    ReviewOutcome::Passed { deferred } => {
                        if !deferred {
                            self.close_group(task);
                        }
                        let commit = self.commit(task, &progress(true, true), true).await;
                        (TaskOutcome::Done, commit)
                    }
                    ReviewOutcome::Skipped { deferred } => {
                        if !deferred {
                            self.close_group(task);
                        }
                        let commit = self.commit(task, &progress(false, true), true).await;
                        (TaskOutcome::Done, commit)
                    }
                    ReviewOutcome::Failed => {
                        let commit = self.commit(task, &progress(true, false), false).await;
                        (TaskOutcome::Failed, commit)
                    }
                }
            }
            BuildOutcome::Cancelled => (TaskOutcome::Cancelled, None),
        }
    }

    /// Runs the commit step and records the group commit of its commit; the
    /// short SHA feeds the task-finished event.
    async fn commit(&self, task: &Task, token: &str, validated: bool) -> Option<String> {
        let commit = CommitStep::new(self.ctx).run(task, token, validated).await;
        if let Some(commit) = &commit {
            self.record_group_commit(task, &commit.full);
        }
        commit.map(|commit| commit.short)
    }

    /// Records the task's batch-review group: a contiguous run of
    /// same-leading-number task IDs in execution order. A changed number
    /// starts a new group: the open-group record decides through
    /// `resume_decision` whether an interrupted group resumes (its base is
    /// restored, so the group's last review still diffs across every commit
    /// of the group) or a fresh one starts at the current HEAD, and the
    /// record is written so the group survives a restart. The group state is
    /// reset at each session start, which makes the decision re-run for the
    /// session's first task.
    async fn note_group(&self, task: &Task) {
        let number = task.group_number();
        let current = self.ctx.group_number();
        if current.is_some() && current == number {
            return;
        }
        let (base, record) = self.group_base(task, number.as_deref()).await;
        if let Some(record) = &record {
            let path = self.ctx.open_group_path();
            if let Err(error) = record.save(path) {
                tracing::warn!("cannot write the open group {}: {error}", path.display());
            }
        }
        self.ctx.set_group(number, base);
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
        let git = self.ctx.git();
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
        match group::OpenGroup::load(self.ctx.open_group_path()) {
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
        let Some(number) = self.ctx.group_number() else {
            return;
        };
        if task.group_number() != Some(number) {
            return;
        }
        let path = self.ctx.open_group_path();
        let Some(mut record) = group::OpenGroup::load(path) else {
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
        if let Err(error) = record.save(path) {
            tracing::warn!("cannot write the open group {}: {error}", path.display());
        }
    }

    /// Deletes the open-group record when the group's last task completed its
    /// review validated; a failed review keeps it so the rerun still diffs
    /// the whole group. A task outside the current group (or without a
    /// leading number) closes nothing.
    fn close_group(&self, task: &Task) {
        let number = task.group_number();
        if number.is_none() || self.ctx.group_number() != number {
            return;
        }
        let path = self.ctx.open_group_path();
        if let Err(error) = group::OpenGroup::clear(path) {
            tracing::warn!("cannot delete the open group {}: {error}", path.display());
        }
    }
}
