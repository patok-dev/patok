use patok_core::complexity::{self, Complexity};
use patok_core::event::NoticeLevel;
use patok_core::pipeline::{Stage, TileStatus};
use patok_core::task::Task;

use super::Engine;
use super::group;
use crate::agents::{
    Agent, FileReviewerAgent, Findings, ReviewHistory, ReviewOutcome, ReviewerAgent, SkipInputs,
    compute_verdict, is_generated, merge_findings, skip_review,
};
use crate::artifact;
use crate::context::SessionContext;
use crate::step::agent_run::{AgentPrompt, StageExit, StageStart, StageWords, run_stage};

/// The largest git diff the reviewer prompt carries:
/// 50 KB; a larger or empty diff falls back to the changed-file list.
const DIFF_LIMIT_BYTES: usize = 50 * 1024;

/// The outcome of one task's review stage.
#[derive(Debug)]
pub(crate) enum ReviewStage {
    /// The review passed: the task is validated.
    Passed,
    /// The review failed: a stage failure with suggestions, leading to WIP.
    Failed,
    /// A reviewer session was cancelled.
    Cancelled,
    /// The stage was skipped by one of the spec's skip rules; the reason was
    /// recorded with the skip notice, and any skip treats the task as
    /// validated.
    Skipped,
}

/// The outcome of the reviewer sessions of one review.
enum ReviewRun {
    /// The final review report and its computed verdict.
    Outcome(ReviewOutcome),
    /// The reviewer session failed: a missing report counts as one HIGH finding.
    Failed,
    /// A reviewer session was cancelled.
    Cancelled,
}

/// How one reviewer session ended other than completing: a cancelled or
/// failed session, before the agent's own validation.
enum ReviewExit {
    Cancelled,
    Failed,
}

impl Engine {
    /// The review stage: decides by the skip
    /// rules in precedence order -- each recorded with its reason and every
    /// skip treating the task as validated -- and, when it runs, drives the
    /// fresh-context reviewer sessions that review and fix in one pass, then
    /// computes the verdict. No crash-recovery checkpoint is written for the
    /// stage.
    pub(crate) async fn review_stage(
        &self,
        task: &Task,
        ctx: &SessionContext,
        plan: Option<&str>,
    ) -> ReviewStage {
        let complexity = complexity::classify(&task.description);
        // The skip decision, recorded with its reason.
        let pending_after: Vec<String> = ctx.task_list().pending_ids();
        let history = ReviewHistory::load(ctx.review_history_path());
        // The raw deferral decides the group's close beside the skip rule:
        // the batch deferral is not the group's last task, and its skip is
        // not a completed review.
        let deferred = group::batch_deferred(&task.id, &pending_after);
        let inputs = SkipInputs {
            review_in_loop: ctx.settings().review_in_loop,
            batch_deferred: ctx.settings().batch_review && deferred,
            skip_review_for_simple: ctx.settings().skip_review_for_simple,
            complexity: Some(complexity),
            builder_clean: true,
            review_confidence_threshold: ctx.settings().review_confidence_threshold,
            history_consecutive_passes: history.match_shape(&task.description, complexity),
            stage_enabled: ctx.review_stage_enabled(),
        };
        if let Some(reason) = skip_review(&inputs) {
            tracing::info!("review skipped for {}: {}", task.id, reason.reason());
            ctx.notice(
                NoticeLevel::Info,
                format!("Review skipped for {}: {}.", task.id, reason.reason()),
            );
            // A skipped stage renders done on the rail like the research and
            // plan stages do; the dash lives in the task-line indicator.
            // A skip other than the batch deferral validated the task, so
            // the group's last task closes the record; the deferral leaves
            // it open for the group's real last task.
            if !deferred {
                self.close_group(&task.id);
            }
            ctx.update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Done));
            return ReviewStage::Skipped;
        }

        // The claims prerequisite gate: a warning only.
        let claims = self.claims_file(&task.id);

        // The changed files: from the working tree,
        // and against the batch group's base so the group's last review spans
        // every commit of the group. No files changed fails the review.
        let base = ctx.group_base();
        let git = ctx.git();
        let changed = git.changed_files(base.as_deref()).await;
        if changed.is_empty() {
            ctx.notice(
                NoticeLevel::Error,
                format!("{}: no changed files to review", task.id),
            );
            ctx.update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Done));
            return ReviewStage::Failed;
        }
        // The reviewer receives the diff when it is non-empty and at most
        // 50 KB, otherwise the changed-file list.
        let diff = git.diff(base.as_deref()).await;
        let diff = (!diff.trim().is_empty() && diff.len() <= DIFF_LIMIT_BYTES).then_some(diff);

        // The review tile is active while the reviewer sessions run (T49.1).
        ctx.update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Active));

        // Applied fixes are inferred from per-file content hashes taken before
        // and after the reviewer, over the same changed-file set.
        let before = self.file_hashes(&changed);
        // Generated files (snapshots, lockfiles) are no unit for a per-file
        // pass: they neither trigger multipass nor get a reviewer of their own,
        // so a snapshot-heavy change does not run dozens of per-file sessions.
        let reviewable: Vec<String> = changed
            .iter()
            .filter(|file| !is_generated(file))
            .cloned()
            .collect();
        let run = if ctx.settings().review_multipass_threshold > 0
            && reviewable.len() as u64 > ctx.settings().review_multipass_threshold
        {
            match multipass_review(ctx, task, &reviewable, &changed).await {
                Ok(outcome) => ReviewRun::Outcome(outcome),
                Err(ReviewExit::Cancelled) => ReviewRun::Cancelled,
                Err(ReviewExit::Failed) => ReviewRun::Failed,
            }
        } else {
            let agent = ReviewerAgent::combined(
                task,
                claims.as_deref(),
                plan,
                &changed,
                diff.as_deref(),
                1,
                ctx.settings().confidence_threshold,
            );
            match one_review(ctx, task, &agent).await {
                Ok(outcome) => ReviewRun::Outcome(outcome),
                Err(ReviewExit::Cancelled) => ReviewRun::Cancelled,
                Err(ReviewExit::Failed) => ReviewRun::Failed,
            }
        };
        let after = self.file_hashes(&changed);
        let applied: Vec<String> = changed
            .iter()
            .zip(before.iter().zip(after.iter()))
            .filter(|(_, (was, now))| was != now)
            .map(|(file, _)| file.clone())
            .collect();
        if applied.is_empty() {
            tracing::info!("the reviewer applied no fixes for {}", task.id);
        } else {
            tracing::info!(
                "the reviewer applied fixes in {} for {}: {}",
                applied.len(),
                task.id,
                applied.join(", ")
            );
        }

        // The verdict.
        let (report, verdict) = match run {
            ReviewRun::Cancelled => {
                ctx.notice(NoticeLevel::Warning, "Review cancelled; nothing committed.");
                ctx.update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Done));
                return ReviewStage::Cancelled;
            }
            ReviewRun::Outcome(outcome) => (outcome.report, outcome.verdict),
            ReviewRun::Failed => {
                // The reviewer agent must succeed: a failed session counts as
                // a missing report (one HIGH finding).
                tracing::error!("the reviewer session failed for {}", task.id);
                (
                    String::new(),
                    compute_verdict(None, ctx.settings().confidence_threshold),
                )
            }
        };
        for below in &verdict.below_threshold {
            tracing::warn!(
                "below-confidence {} finding for {} (confidence {:.2}): {}",
                below.severity,
                task.id,
                below.finding.confidence,
                below.finding.issue
            );
        }
        let review_path = self.write_review_file(&task.id, &report);
        // The pass/fail outcome is recorded in the persistent learned-confidence
        // history in the background.
        self.record_review_outcome(task, complexity, verdict.passed);

        ctx.update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Done));
        if verdict.passed {
            ctx.notice(NoticeLevel::Info, format!("Review passed for {}.", task.id));
            // The group's last task completed its review validated: the
            // record goes. A failed review keeps it so the rerun still
            // diffs the whole group.
            if !deferred {
                self.close_group(&task.id);
            }
            ReviewStage::Passed
        } else {
            let report = review_path
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "the review report".into());
            ctx.notice(
                NoticeLevel::Error,
                format!(
                    "review failed for {}: HIGH/MEDIUM issues remain, check report at {}",
                    task.id, report
                ),
            );
            ReviewStage::Failed
        }
    }

    /// The SHA-256 content hashes of `files` (missing files hash to none):
    /// taken before and after the reviewer, they infer the applied fixes.
    fn file_hashes(&self, files: &[String]) -> Vec<Option<String>> {
        use sha2::{Digest, Sha256};
        files
            .iter()
            .map(|file| {
                std::fs::read(self.inner.config.project_dir.join(file))
                    .ok()
                    .map(|bytes| format!("{:x}", Sha256::digest(&bytes)))
            })
            .collect()
    }

    /// Records one review outcome in the persistent learned-confidence history
    /// in the background, for the
    /// simple and medium shapes the feature matches on.
    fn record_review_outcome(&self, task: &Task, complexity: Complexity, passed: bool) {
        if complexity == Complexity::Complex {
            return;
        }
        let description = task.description.clone();
        let path = self.inner.config.review_history.clone();
        let date = artifact::unix_now().as_secs().to_string();
        tokio::spawn(async move {
            let mut history = ReviewHistory::load(&path);
            history.record(&description, complexity, passed, &date);
            if let Err(error) = history.save(&path) {
                tracing::warn!(
                    "cannot write the review history {}: {error}",
                    path.display()
                );
            }
        });
    }
}

/// One reviewer session: announced as the active agent (T11.1), recorded
/// with the reviewer allowlist, its raw stream written to the history log,
/// and its captured final message validated by the agent (the combined and
/// integration reviewers compute the verdict over it, the per-file
/// reviewer parses its findings out of it).
async fn one_review<A: Agent>(
    ctx: &SessionContext,
    task: &Task,
    agent: &A,
) -> Result<A::Session, ReviewExit> {
    // The starting line names the provider the review actually runs on
    // (mistral when the reviewer provider is mistral), so the reviewer
    // pair resolves first (T78.1).
    let (provider, model) = ctx.reviewer();
    let exit = run_stage(
        ctx,
        &provider,
        model.as_deref(),
        StageStart::Announce,
        agent.role(),
        AgentPrompt::of(agent),
        StageWords {
            capture: true,
            could_not_start: format!("reviewer could not start for {}", task.id),
            default_failure: "review failed".into(),
            cancelled_notice: None,
            failure_level: NoticeLevel::Error,
            failure_prefix: format!("review for {}: ", task.id),
            failure_suffix: String::new(),
        },
    )
    .await;
    match exit {
        StageExit::Completed(events) => agent.validate(&events).map_err(|_| ReviewExit::Failed),
        StageExit::Cancelled => Err(ReviewExit::Cancelled),
        StageExit::Failed => Err(ReviewExit::Failed),
    }
}

/// The multipass review: one report-only reviewer
/// session per changed file (a failed file review is skipped, the loop
/// continues), then one integration reviewer session over the merged,
/// de-duplicated findings that produces the final report.
async fn multipass_review(
    ctx: &SessionContext,
    task: &Task,
    reviewable: &[String],
    changed: &[String],
) -> Result<ReviewOutcome, ReviewExit> {
    let mut merged = Findings::default();
    for file in reviewable {
        let agent = FileReviewerAgent::new(task, file, 1);
        match one_review(ctx, task, &agent).await {
            Err(ReviewExit::Cancelled) => return Err(ReviewExit::Cancelled),
            Err(ReviewExit::Failed) => {
                // A failed file review is skipped and the loop continues.
                continue;
            }
            Ok(Some(findings)) => merge_findings(&mut merged, findings),
            Ok(None) => {}
        }
    }
    let findings_json = serde_json::to_string_pretty(&merged).unwrap_or_default();
    let agent = ReviewerAgent::integration(
        task,
        &findings_json,
        changed,
        2,
        ctx.settings().confidence_threshold,
    );
    one_review(ctx, task, &agent).await
}
