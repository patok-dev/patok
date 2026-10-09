use patok_core::complexity::{self, Complexity};
use patok_core::event::{AgentRole, NoticeLevel};
use patok_core::pipeline::{Stage, TileStatus};
use patok_core::review::Severity;
use patok_core::task::Task;
use patok_providers::REVIEWER_TOOLS;
use tokio_util::sync::CancellationToken;

use super::Engine;
use super::session::{AgentPrompt, StageExit, StageStart, StageWords};
use super::state::Unit;
use crate::{artifact, plan, prompt, review, taskfile};

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
    /// The final review report.
    Report(String),
    /// The reviewer session failed: a missing report counts as one HIGH finding.
    Failed,
    /// A reviewer session was cancelled.
    Cancelled,
}

/// Merges `findings` into `merged`, de-duplicated by file and issue.
fn merge_findings(merged: &mut review::Findings, findings: review::Findings) {
    for severity in [Severity::High, Severity::Medium, Severity::Low] {
        let bucket = match severity {
            Severity::High => &mut merged.high,
            Severity::Medium => &mut merged.medium,
            Severity::Low => &mut merged.low,
        };
        for finding in findings.bucket(severity) {
            if !bucket
                .iter()
                .any(|existing| existing.file == finding.file && existing.issue == finding.issue)
            {
                bucket.push(finding.clone());
            }
        }
    }
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
        unit: &Unit,
        cancel: &CancellationToken,
        plan: Option<&str>,
    ) -> ReviewStage {
        let complexity = complexity::classify(&task.description);
        // The skip decision, recorded with its reason.
        let pending_after: Vec<String> = taskfile::load(&self.inner.config.task_file())
            .unwrap_or_default()
            .into_iter()
            .filter(|t| !t.done && !t.is_malformed())
            .map(|t| t.id)
            .collect();
        let history = review::ReviewHistory::load(&self.inner.config.review_history);
        // The raw deferral decides the group's close beside the skip rule:
        // the batch deferral is not the group's last task, and its skip is
        // not a completed review.
        let deferred = review::batch_deferred(&task.id, &pending_after);
        let inputs = review::SkipInputs {
            review_in_loop: unit.review_in_loop,
            batch_deferred: unit.batch_review && deferred,
            skip_review_for_simple: unit.skip_review_for_simple,
            complexity: Some(complexity),
            builder_clean: true,
            review_confidence_threshold: unit.review_confidence_threshold,
            history_consecutive_passes: history.match_shape(&task.description, complexity),
            stage_enabled: unit.review_stage_enabled,
        };
        if let Some(reason) = review::skip_review(&inputs) {
            tracing::info!("review skipped for {}: {}", task.id, reason.reason());
            self.notice(
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
            self.update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Done));
            return ReviewStage::Skipped;
        }

        // The claims prerequisite gate: a warning only.
        let claims = self.claims_file(&task.id);

        // The changed files: from the working tree,
        // and against the batch group's base so the group's last review spans
        // every commit of the group. No files changed fails the review.
        let base = self.state().group_base.clone();
        let git = self.git();
        let changed = git.changed_files(base.as_deref()).await;
        if changed.is_empty() {
            self.notice(
                NoticeLevel::Error,
                format!("{}: no changed files to review", task.id),
            );
            self.update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Done));
            return ReviewStage::Failed;
        }
        // The reviewer receives the diff when it is non-empty and at most
        // 50 KB, otherwise the changed-file list.
        let diff = git.diff(base.as_deref()).await;
        let diff = (!diff.trim().is_empty() && diff.len() <= DIFF_LIMIT_BYTES).then_some(diff);

        // The review tile is active while the reviewer sessions run (T49.1).
        self.update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Active));

        // Applied fixes are inferred from per-file content hashes taken before
        // and after the reviewer, over the same changed-file set.
        let before = self.file_hashes(&changed);
        // Generated files (snapshots, lockfiles) are no unit for a per-file
        // pass: they neither trigger multipass nor get a reviewer of their own,
        // so a snapshot-heavy change does not run dozens of per-file sessions.
        let reviewable: Vec<String> = changed
            .iter()
            .filter(|file| !review::is_generated(file))
            .cloned()
            .collect();
        let run = if unit.review_multipass_threshold > 0
            && reviewable.len() as u64 > unit.review_multipass_threshold
        {
            self.multipass_review(task, unit, cancel, &reviewable, &changed)
                .await
        } else {
            let prompt =
                prompt::reviewer(task, claims.as_deref(), plan, &changed, diff.as_deref(), 1);
            self.one_review(task, unit, cancel, &prompt).await
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

        let result = match run {
            ReviewRun::Cancelled => {
                self.notice(NoticeLevel::Warning, "Review cancelled; nothing committed.");
                self.update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Done));
                return ReviewStage::Cancelled;
            }
            ReviewRun::Report(report) => (report, true),
            ReviewRun::Failed => {
                // The reviewer agent must succeed: a failed session counts as
                // a missing report (one HIGH finding).
                tracing::error!("the reviewer session failed for {}", task.id);
                (String::new(), false)
            }
        };

        // The verdict.
        let report = result.0;
        let verdict = review::compute_verdict(
            result.1.then_some(report.as_str()),
            unit.confidence_threshold,
        );
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

        self.update_tiles(|tiles| tiles.set(Stage::Review, TileStatus::Done));
        if verdict.passed {
            self.notice(NoticeLevel::Info, format!("Review passed for {}.", task.id));
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
            self.notice(
                NoticeLevel::Error,
                format!(
                    "review failed for {}: HIGH/MEDIUM issues remain, check report at {}",
                    task.id, report
                ),
            );
            ReviewStage::Failed
        }
    }

    /// The reviewer session the review runs on when the reviewer provider names
    /// a provider this build implements (`reviewer_provider`): that provider,
    /// resolved like any other unit of work. A provider that supports the
    /// reviewer model (the claude provider) runs with the configured one
    /// (`reviewer_model`); the other providers keep their default model.
    fn reviewer_unit(&self, unit: &Unit) -> Unit {
        let mut reviewer = unit.clone();
        if let Some(kind) = reviewer.reviewer_provider.kind() {
            let name = reviewer.reviewer_provider;
            match self.inner.resolver.as_ref() {
                Some(resolver) => match resolver(kind) {
                    Ok(provider) => {
                        reviewer.provider = provider;
                        reviewer.model = reviewer
                            .provider
                            .supports_reviewer_model()
                            .then(|| reviewer.reviewer_model.clone());
                    }
                    Err(error) => {
                        tracing::warn!(
                            "the {name} reviewer provider is unavailable ({error}); \
                             the reviewer runs on the configured provider"
                        );
                    }
                },
                None => {
                    tracing::warn!(
                        "the {name} reviewer provider has no resolver; \
                         the reviewer runs on the fixed provider"
                    );
                }
            }
        }
        reviewer
    }

    /// One reviewer session: announced as the active agent (T11.1), recorded
    /// with the reviewer allowlist, its raw stream written to the history log,
    /// and its captured final message taken as the review report.
    async fn one_review(
        &self,
        task: &Task,
        unit: &Unit,
        cancel: &CancellationToken,
        prompt: &str,
    ) -> ReviewRun {
        // The starting line names the provider the review actually runs on
        // (mistral when the reviewer provider is mistral), so the unit resolves first
        // (T78.1).
        let reviewer_unit = self.reviewer_unit(unit);
        let exit = self
            .run_stage(
                StageStart::Announce,
                AgentRole::Reviewer,
                AgentPrompt {
                    prompt: prompt.to_string(),
                    system_prompt: prompt::REVIEWER_SYSTEM,
                    label: "review".into(),
                    tools: REVIEWER_TOOLS,
                },
                cancel.clone(),
                &reviewer_unit,
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
            StageExit::Completed(events) => ReviewRun::Report(plan::captured_text(&events)),
            StageExit::Cancelled => ReviewRun::Cancelled,
            StageExit::Failed => ReviewRun::Failed,
        }
    }

    /// The multipass review: one report-only reviewer
    /// session per changed file (a failed file review is skipped, the loop
    /// continues), then one integration reviewer session over the merged,
    /// de-duplicated findings that produces the final report.
    async fn multipass_review(
        &self,
        task: &Task,
        unit: &Unit,
        cancel: &CancellationToken,
        reviewable: &[String],
        changed: &[String],
    ) -> ReviewRun {
        let mut merged = review::Findings::default();
        for file in reviewable {
            let prompt = prompt::reviewer_file(task, file, 1);
            let run = self.one_review(task, unit, cancel, &prompt).await;
            match run {
                ReviewRun::Cancelled => return ReviewRun::Cancelled,
                ReviewRun::Failed => {
                    // A failed file review is skipped and the loop continues.
                    continue;
                }
                ReviewRun::Report(report) => {
                    if let Some(findings) = review::parse_report(&report).findings {
                        merge_findings(&mut merged, findings);
                    }
                }
            }
        }
        let findings_json = serde_json::to_string_pretty(&merged).unwrap_or_default();
        let prompt = prompt::reviewer_integration(task, &findings_json, changed, 2);
        self.one_review(task, unit, cancel, &prompt).await
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
            let mut history = review::ReviewHistory::load(&path);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipass_findings_merge_and_de_duplicate_by_file_and_issue() {
        let mut merged = review::Findings::default();
        let finding = |file: &str, issue: &str| review::Finding {
            file: file.into(),
            issue: issue.into(),
            confidence: 0.9,
            ..review::Finding::default()
        };
        for (file, issue) in [
            ("a.rs", "a leak"),
            ("a.rs", "a leak"),
            ("a.rs", "another"),
            ("b.rs", "a leak"),
        ] {
            let mut findings = review::Findings::default();
            findings.high.push(finding(file, issue));
            merge_findings(&mut merged, findings);
        }
        assert_eq!(merged.high.len(), 3, "{merged:?}");
        assert_eq!(merged.medium.len(), 0);
        assert!(
            merged
                .high
                .iter()
                .any(|f| f.file == "a.rs" && f.issue == "another")
        );
        // The same finding in a different severity bucket is its own entry.
        let mut findings = review::Findings::default();
        findings.medium.push(finding("a.rs", "a leak"));
        merge_findings(&mut merged, findings);
        assert_eq!(merged.medium.len(), 1);
    }
}
