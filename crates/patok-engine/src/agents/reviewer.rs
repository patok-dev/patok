//! The reviewer agents: the reviewer prompts, the review report's verdict
//! and findings parsing, the verdict computation, the multipass findings
//! merge, the skip decision in precedence order, the persistent
//! learned-confidence history, and the agents that drive the combined,
//! per-file and integration review sessions.

use std::collections::HashSet;
use std::path::Path;

use patok_core::complexity::Complexity;
use patok_core::config::{EnumSpelling, Severity};
use patok_core::event::{AgentEvent, AgentRole};
use patok_core::task::Task;
use patok_providers::{REVIEWER_TOOLS, Tool};
use serde::{Deserialize, Serialize};

use super::{Agent, SPEC_FILE, captured_text};
use crate::TASK_FILE;

/// Appended to the CLI's own system prompt for the review stage's
/// reviewer sessions.
const REVIEWER_SYSTEM: &str = "You are the Reviewer agent in an autonomous build loop. \
Review and validate these claims. Find the gaps. \
You are a fresh-context combined review-and-fix agent: you did not build this work, and nothing about it is trusted until you verified it. \
You may read files, search the project, edit and write files and run shell commands. \
Do not modify the spec file, the task list or the project instruction file, and write no file other than your review report. \
First pass: thorough validation. Second pass: assume bugs remain and find what was missed.";

/// The review report every reviewer session must end its final message with
///: a verdict line and the findings JSON.
const REVIEW_REPORT: &str = "End your final message with the review report, and nothing after it:\n\n\
Verdict: PASS\n\n\
```json\n\
{\"high\": [], \"medium\": [], \"low\": []}\n\
```\n\n\
The verdict line is exactly `Verdict: PASS` when the work is validated and `Verdict: FAIL` when it is not. \
The JSON block holds one array per severity: high, medium and low. Each finding carries its severity (the bucket it sits in), the file, the line, the issue, whether you fixed it (\"fixed\": true or false), a category, the source evidence and a confidence between 0.0 and 1.0. \
An empty array is correct when you found nothing at that severity. The report is the last thing in your final message.";

/// The reviewer's job in order and the severity calibration, shared by the reviewer prompt variants.
const REVIEW_RULES: &str = "Your job, in order:\n\n\
1. Read the build claims (the fallback above when they are missing) and verify every claim against the code. The builder's claims are untrusted assertions: check each one against what the code actually does.\n\
2. Run the project's build and tests yourself, independently; never trust the builder's verification. Stack checks: Rust (cargo check, cargo clippy, cargo test), Python (compile and pytest), Node/TS (type-check and tests), Docker (config only, never start services); skip a check with a reason when the tool is unavailable.\n\
3. Scrutinize the Gaps and Assumptions: uncertainties and untested edge cases are where bugs hide.\n\
4. Fix every HIGH and MEDIUM issue you find, surgically, in the same pass: fix exactly the issue and nothing around it, no surrounding refactors, no separate fix loop. LOW issues are report-only.\n\
5. Re-run the checks after fixing.\n\n\
Severity calibration: HIGH (security holes, unhandled failures on external or user input, logic errors, crash paths) is always reported and fixed; MEDIUM (missing error handling at trust boundaries, resource leaks, off-by-one) is reported and fixed; LOW (style consistent with the codebase, local naming) is reported only. Borderlines: an unhandled error on user-controlled input is HIGH, not MEDIUM; an ignored return value with no production effect is LOW, not MEDIUM.\n\n\
Report bugs, panics, security issues, logic errors, missing boundary error handling, claims that contradict the code, races and leaks. Skip style, minor naming, missing docs, conventions matching the project and theoretical improvements.";

/// The user prompt of the review stage's reviewer session: a fresh-context combined review-and-fix agent. The
/// user prompt carries the task, the build claims (with the plan and the
/// changed-file list as the labelled fallback when the claims are missing),
/// the git diff when it is non-empty and at most 50 KB (otherwise the
/// changed-file list), the pass number and the spec and task file names.
fn reviewer(
    task: &Task,
    claims: Option<&str>,
    plan: Option<&str>,
    changed_files: &[String],
    diff: Option<&str>,
    pass: u32,
) -> String {
    let claims_block = match claims {
        Some(claims) => format!(
            "The build claims the builder wrote (untrusted; verify every claim):\n\n{claims}"
        ),
        None => format!(
            "No build claims were found; the plan the builder worked from and the changed files are the fallback:\n\n{plan}\n\nChanged files:\n{files}",
            plan = plan.unwrap_or("no plan was recorded"),
            files = file_list(changed_files),
        ),
    };
    let changes_block = match diff {
        Some(diff) => format!("The git diff of the work under review:\n\n```diff\n{diff}\n```"),
        None => format!(
            "The diff is empty or too large to carry; the changed files are:\n\n{files}",
            files = file_list(changed_files),
        ),
    };
    format!(
        "Review and validate the work done for the task below.\n\n\
         Task {id}: {description}\n\n\
         The full task list is in {TASK_FILE} for context; treat it as read-only. The spec is {SPEC_FILE} when it exists in the project root.\n\n\
         {claims_block}\n\n\
         {changes_block}\n\n\
         Pass {pass}.\n\n\
         {REVIEW_RULES}\n\n\
         {REVIEW_REPORT}",
        id = task.id,
        description = task.description,
    )
}

/// The per-file reviewer session of a multipass review: one file at a time, single-file concerns only, and report-only -- the
/// integration session decides what gets fixed.
fn reviewer_file(task: &Task, file: &str, pass: u32) -> String {
    format!(
        "Review one file of the work done for the task below.\n\n\
         Task {id}: {description}\n\n\
         The file to examine is exactly this one: {file}. Review it on its own merits: single-file concerns only (bugs, panics, security issues, logic errors, missing boundary error handling inside the file). Cross-file and integration concerns belong to the integration pass, not this one.\n\n\
         Do not edit any file: this pass is report-only. Every finding you report carries \"fixed\": false.\n\n\
         The spec is {SPEC_FILE} when it exists in the project root; the task list is {TASK_FILE}, both read-only.\n\n\
         Pass {pass}.\n\n\
         Report bugs, panics, security issues, logic errors and missing boundary error handling; skip style, minor naming, missing docs and conventions matching the project.\n\n\
         {REVIEW_REPORT}",
        id = task.id,
        description = task.description,
    )
}

/// The integration reviewer session of a multipass review: it receives the merged, de-duplicated per-file findings and
/// focuses on what per-file passes cannot see -- cross-file issues -- before
/// producing the final report.
fn reviewer_integration(
    task: &Task,
    findings_json: &str,
    changed_files: &[String],
    pass: u32,
) -> String {
    format!(
        "Review the integration of the work done for the task below; you are the final pass.\n\n\
         Task {id}: {description}\n\n\
         The per-file reviews already reported these findings (merged and de-duplicated):\n\n\
         ```json\n{findings_json}\n```\n\n\
         Do not re-report those findings; they are already recorded. Focus on what the per-file passes cannot see: cross-file issues, wiring, contracts between files, and anything in the gaps of the work.\n\n\
         The changed files are:\n{files}\n\n\
         The spec is {SPEC_FILE} when it exists in the project root; the task list is {TASK_FILE}, both read-only.\n\n\
         Pass {pass}.\n\n\
         {REVIEW_RULES}\n\n\
         {REVIEW_REPORT}",
        id = task.id,
        description = task.description,
        files = file_list(changed_files),
    )
}

/// One changed file per line, as a bulleted list.
fn file_list(files: &[String]) -> String {
    files
        .iter()
        .map(|file| format!("- {file}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One finding of the review report's JSON block. Every field is optional at the JSON level -- the
/// reviewer is an LLM -- but the severity buckets are positional.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Finding {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub line: Option<u64>,
    #[serde(default)]
    pub issue: String,
    #[serde(default)]
    pub fixed: bool,
    #[serde(default)]
    pub category: String,
    /// The source evidence the finding rests on.
    #[serde(default)]
    pub source: String,
    /// The finding's confidence, 0.0 to 1.0.
    #[serde(default)]
    pub confidence: f64,
}

/// The findings JSON block of a review report: one bucket per severity.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Findings {
    #[serde(default)]
    pub high: Vec<Finding>,
    #[serde(default)]
    pub medium: Vec<Finding>,
    #[serde(default)]
    pub low: Vec<Finding>,
}

impl Findings {
    /// The findings of `severity`.
    pub(crate) fn bucket(&self, severity: Severity) -> &[Finding] {
        match severity {
            Severity::High => &self.high,
            Severity::Medium => &self.medium,
            Severity::Low => &self.low,
        }
    }
}

/// Merges `findings` into `merged`, de-duplicated by file and issue.
pub(crate) fn merge_findings(merged: &mut Findings, findings: Findings) {
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

/// A parsed review report: the case-insensitive `Verdict: PASS` line, when
/// present, and the findings JSON block.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ReportParse {
    /// `Some(true)` for a pass verdict line, `Some(false)` for fail, `None`
    /// when the report has no verdict line.
    pub verdict_pass: Option<bool>,
    /// `None` when the report has no valid findings JSON block.
    pub findings: Option<Findings>,
}

/// Parses a review report: the last `Verdict: PASS`/`Verdict: FAIL` line
/// (case-insensitive) and the findings JSON block -- a fenced block, or the
/// last balanced JSON object in the report that names at least one of the
/// high/medium/low buckets. A report with neither is still a parse (both
/// fields `None`); an absent or invalid JSON block counts as one HIGH finding
/// in the verdict computation.
pub(crate) fn parse_report(text: &str) -> ReportParse {
    let verdict_pass = text.lines().rev().find_map(|line| {
        let verdict = line.trim_start().to_ascii_lowercase();
        let rest = verdict
            .strip_prefix("verdict:")?
            .trim()
            .trim_end_matches('.');
        match rest {
            "pass" | "passed" => Some(true),
            "fail" | "failed" => Some(false),
            _ => None,
        }
    });
    ReportParse {
        verdict_pass,
        findings: extract_findings(text),
    }
}

/// The findings JSON block of the report: the last balanced JSON object that
/// names at least one severity bucket and parses.
fn extract_findings(text: &str) -> Option<Findings> {
    balanced_objects(text)
        .iter()
        .rev()
        .filter(|candidate| {
            Severity::SPELLINGS
                .iter()
                .any(|severity| candidate.contains(&format!("\"{severity}\"")))
        })
        .filter_map(|candidate| serde_json::from_str::<Findings>(candidate).ok())
        .next()
}

/// Every balanced JSON object in `text` (brace matching that respects string
/// literals and escapes), as candidate findings blocks.
fn balanced_objects(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut objects = Vec::new();
    let mut index = 0;
    while let Some(open) = bytes[index..].iter().position(|&b| b == b'{') {
        let start = index + open;
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, &byte) in bytes[start..].iter().enumerate() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        objects.push(&text[start..=start + offset]);
                        index = start + offset + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if depth != 0 {
            break;
        }
        if index <= start {
            index = start + 1;
        }
    }
    objects
}

/// One finding counted against the verdict, with the bucket it came from.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QualifyingFinding {
    pub severity: Severity,
    pub finding: Finding,
}

/// The computed verdict of one review.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Verdict {
    pub passed: bool,
    /// HIGH and MEDIUM findings at or above the confidence threshold.
    pub qualifying: Vec<QualifyingFinding>,
    /// Findings below the confidence threshold, logged for manual review.
    pub below_threshold: Vec<QualifyingFinding>,
    /// The number of HIGH findings counted (an absent or invalid findings
    /// block is one).
    pub high: usize,
    /// The number of MEDIUM findings counted.
    pub medium: usize,
}

/// Computes the review's verdict: the review passes when its report says PASS,
/// or when it holds zero HIGH and zero MEDIUM findings at or above the
/// confidence threshold. A missing or empty report, or an absent or invalid
/// findings JSON block, counts as one HIGH finding -- so only a PASS verdict
/// can carry those. Findings below the threshold are excluded from the
/// computation and returned for manual review, whatever their severity.
pub(crate) fn compute_verdict(report: Option<&str>, threshold: f64) -> Verdict {
    let Some(report) = report.filter(|report| !report.trim().is_empty()) else {
        return Verdict {
            passed: false,
            qualifying: vec![missing_report()],
            below_threshold: Vec::new(),
            high: 1,
            medium: 0,
        };
    };
    let parsed = parse_report(report);
    let Some(findings) = parsed.findings else {
        return Verdict {
            passed: parsed.verdict_pass == Some(true),
            qualifying: vec![missing_report()],
            below_threshold: Vec::new(),
            high: 1,
            medium: 0,
        };
    };
    let mut verdict = Verdict {
        passed: parsed.verdict_pass == Some(true),
        qualifying: Vec::new(),
        below_threshold: Vec::new(),
        high: 0,
        medium: 0,
    };
    for severity in [Severity::High, Severity::Medium] {
        for finding in findings.bucket(severity) {
            let counted = QualifyingFinding {
                severity,
                finding: finding.clone(),
            };
            if finding.confidence >= threshold {
                verdict.qualifying.push(counted);
                if severity == Severity::High {
                    verdict.high += 1;
                } else {
                    verdict.medium += 1;
                }
            } else {
                verdict.below_threshold.push(counted);
            }
        }
    }
    for finding in findings.bucket(Severity::Low) {
        verdict.below_threshold.push(QualifyingFinding {
            severity: Severity::Low,
            finding: finding.clone(),
        });
    }
    if verdict.high == 0 && verdict.medium == 0 {
        verdict.passed = true;
    }
    verdict
}

/// Whether `file` is generated output (an insta snapshot or a lockfile): it
/// has no single-file concerns for a per-file reviewer to find.
pub(crate) fn is_generated(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    name.ends_with(".snap")
        || name.ends_with(".snap.new")
        || name.ends_with(".lock")
        || name == "package-lock.json"
}

/// The single HIGH finding an absent or invalid findings block counts as.
fn missing_report() -> QualifyingFinding {
    QualifyingFinding {
        severity: Severity::High,
        finding: Finding {
            issue: "the review report has no valid findings JSON block".into(),
            category: "report".into(),
            confidence: 1.0,
            ..Finding::default()
        },
    }
}

/// Why the review stage was skipped, in the spec's precedence order: each variant carries the reason recorded with the skip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// `review_in_loop` is off: the task is validated by the builder's own
    /// verification alone.
    ReviewInLoopOff,
    /// Batch review defers the review of every task in a group to the group's
    /// last task.
    BatchDeferred,
    /// A Simple task whose builder verification exited cleanly, under
    /// `skip_review_for_simple`.
    SimpleWithPassingBuild,
    /// A similar-shaped past task cluster reached the review-confidence
    /// threshold of consecutive review passes.
    LearnedConfidence,
    /// The review stage is disabled in the pipeline stage list.
    StageDisabled,
}

impl SkipReason {
    /// The human-readable reason recorded with the skip.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::ReviewInLoopOff => {
                "review in loop is off; the task is validated by the builder's own verification"
            }
            Self::BatchDeferred => "batch review defers the review to the group's last task",
            Self::SimpleWithPassingBuild => {
                "a simple task whose builder verification exited cleanly; skip review for simple"
            }
            Self::LearnedConfidence => "learned confidence: similar tasks passed review repeatedly",
            Self::StageDisabled => "the review stage is disabled",
        }
    }
}

/// The inputs of the review skip decision, snapshotted per task.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SkipInputs {
    pub review_in_loop: bool,
    /// The task is deferred by batch review: a pending task after it shares its
    /// leading task-ID number.
    pub batch_deferred: bool,
    pub skip_review_for_simple: bool,
    pub complexity: Option<Complexity>,
    /// The builder's verification exited cleanly.
    pub builder_clean: bool,
    pub review_confidence_threshold: u64,
    /// The consecutive review passes of the matching history cluster.
    pub history_consecutive_passes: Option<u32>,
    pub stage_enabled: bool,
}

/// Decides whether the review runs, recording why when it does not.
pub(crate) fn skip_review(inputs: &SkipInputs) -> Option<SkipReason> {
    if !inputs.review_in_loop {
        return Some(SkipReason::ReviewInLoopOff);
    }
    if inputs.batch_deferred {
        return Some(SkipReason::BatchDeferred);
    }
    if inputs.skip_review_for_simple
        && inputs.complexity == Some(Complexity::Simple)
        && inputs.builder_clean
    {
        return Some(SkipReason::SimpleWithPassingBuild);
    }
    if inputs
        .complexity
        .is_some_and(|tier| tier != Complexity::Complex)
        && inputs.review_confidence_threshold > 0
        && inputs.history_consecutive_passes.is_some_and(|passes| {
            passes >= u32::try_from(inputs.review_confidence_threshold).unwrap_or(u32::MAX)
        })
    {
        return Some(SkipReason::LearnedConfidence);
    }
    if !inputs.stage_enabled {
        return Some(SkipReason::StageDisabled);
    }
    None
}

/// The clusters cap of the persistent learned-confidence history.
const CLUSTER_CAP: usize = 200;
/// The Jaccard word-overlap similarity at or above which a cluster matches.
const MATCH_SIMILARITY: f64 = 0.4;
/// The representative description is the first this many characters of the
/// normalised task text.
const REPRESENTATIVE_CHARS: usize = 60;

/// One learned-confidence cluster: a task shape with its review outcome counts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Cluster {
    /// The first 60 characters of the normalised task text.
    representative: String,
    /// The normalised word set, for keyword matching.
    tokens: Vec<String>,
    /// "simple" or "medium".
    tier: String,
    passes: u32,
    fails: u32,
    consecutive_passes: u32,
    /// The date of the last failure.
    last_fail: Option<String>,
}

impl Cluster {
    /// The Jaccard similarity of the cluster's word set and `tokens`.
    fn similarity(&self, tokens: &HashSet<String>) -> f64 {
        let own: HashSet<&String> = self.tokens.iter().collect();
        let other: HashSet<&String> = tokens.iter().collect();
        let shared = own.intersection(&other).count();
        let union = own.union(&other).count();
        if union == 0 {
            0.0
        } else {
            shared as f64 / union as f64
        }
    }
}

/// The persistent learned-confidence history: stored globally per user as JSON, matched by complexity tier
/// plus Jaccard word overlap (keyword mode; no embedding service exists yet).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ReviewHistory {
    clusters: Vec<Cluster>,
}

impl ReviewHistory {
    /// Loads the history from `path`; a missing file is an empty history.
    pub(crate) fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Saves the history to `path`, best-effort: the error is returned for the
    /// caller to log.
    pub(crate) fn save(&self, path: &Path) -> std::io::Result<()> {
        patok_core::utils::atomic_write::write_atomic(
            path,
            &serde_json::to_string(self).unwrap_or_default(),
        )
    }

    /// The consecutive review passes of the best-matching cluster for the task
    /// shape: the same tier (Simple or Medium only) and Jaccard word overlap
    /// at or above the matching similarity. `None` when nothing matches.
    pub(crate) fn match_shape(&self, description: &str, tier: Complexity) -> Option<u32> {
        if tier == Complexity::Complex {
            return None;
        }
        let tokens = normalize(description);
        let tier = tier_name(tier)?;
        self.clusters
            .iter()
            .filter(|cluster| cluster.tier == tier)
            .filter(|cluster| cluster.similarity(&tokens) >= MATCH_SIMILARITY)
            .map(|cluster| cluster.consecutive_passes)
            .max()
    }

    /// Records one review outcome for the task shape: a pass increments the
    /// passes and the consecutive passes; a fail increments the fails, resets
    /// the consecutive passes and stores the date. With no match a new cluster
    /// is created; at the cap the least-observed cluster is evicted.
    pub(crate) fn record(&mut self, description: &str, tier: Complexity, passed: bool, date: &str) {
        let Some(tier) = tier_name(tier) else {
            return;
        };
        let tokens = normalize(description);
        let best = self
            .clusters
            .iter_mut()
            .filter(|cluster| cluster.tier == tier)
            .max_by(|a, b| {
                a.similarity(&tokens)
                    .partial_cmp(&b.similarity(&tokens))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        match best.filter(|cluster| cluster.similarity(&tokens) >= MATCH_SIMILARITY) {
            Some(cluster) => {
                if passed {
                    cluster.passes += 1;
                    cluster.consecutive_passes += 1;
                } else {
                    cluster.fails += 1;
                    cluster.consecutive_passes = 0;
                    cluster.last_fail = Some(date.into());
                }
            }
            None => {
                if self.clusters.len() >= CLUSTER_CAP {
                    let least_observed = self
                        .clusters
                        .iter()
                        .enumerate()
                        .min_by_key(|(index, cluster)| (cluster.passes + cluster.fails, *index))
                        .map(|(index, _)| index);
                    if let Some(index) = least_observed {
                        self.clusters.remove(index);
                    }
                }
                let representative: String = normalize_join(description)
                    .chars()
                    .take(REPRESENTATIVE_CHARS)
                    .collect();
                self.clusters.push(Cluster {
                    representative,
                    tokens: tokens.into_iter().collect(),
                    tier: tier.into(),
                    passes: u32::from(passed),
                    fails: u32::from(!passed),
                    consecutive_passes: u32::from(passed),
                    last_fail: (!passed).then(|| date.into()),
                });
            }
        }
    }
}

/// The stored spelling of a tier; `None` for Complex (never clustered).
fn tier_name(tier: Complexity) -> Option<&'static str> {
    match tier {
        Complexity::Simple => Some("simple"),
        Complexity::Medium => Some("medium"),
        Complexity::Complex => None,
    }
}

/// The significant words of a task description, lowercased and de-duplicated.
fn normalize(description: &str) -> HashSet<String> {
    description
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty() && !STOPWORDS.contains(token))
        .map(str::to_string)
        .collect()
}

/// The normalised text with stop words removed, for the representative.
fn normalize_join(description: &str) -> String {
    description
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty() && !STOPWORDS.contains(token))
        .collect::<Vec<_>>()
        .join(" ")
}

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "has", "have", "in",
    "into", "is", "it", "its", "of", "on", "or", "that", "the", "their", "then", "there", "these",
    "this", "to", "was", "were", "when", "which", "with",
];

/// The payload one reviewer session produces: the report as captured and its
/// computed verdict. The verdict's `passed` flag is the accept/reject; the
/// report and the below-threshold findings travel with it even on a fail.
pub(crate) struct ReviewOutcome {
    /// The review report as captured.
    pub report: String,
    /// The computed verdict (passed flag, qualifying and below-threshold findings).
    pub verdict: Verdict,
}

/// The kind of reviewer session one [`ReviewerAgent`] drives.
enum ReviewKind {
    /// The single-pass combined review-and-fix session, and the multipass
    /// review's integration session.
    Combined {
        claims: Option<String>,
        plan: Option<String>,
        changed: Vec<String>,
        diff: Option<String>,
    },
    /// The multipass review's integration session over the merged per-file
    /// findings.
    Integration {
        findings_json: String,
        changed: Vec<String>,
    },
}

/// The review stage's combined and integration reviewer agent: its session's
/// final message is the review report, and its validation computes the
/// verdict over it.
pub(crate) struct ReviewerAgent {
    task: Task,
    kind: ReviewKind,
    pass: u32,
    threshold: f64,
}

impl ReviewerAgent {
    /// The single-pass combined reviewer (pass 1).
    pub(crate) fn combined(
        task: &Task,
        claims: Option<&str>,
        plan: Option<&str>,
        changed: &[String],
        diff: Option<&str>,
        pass: u32,
        threshold: f64,
    ) -> Self {
        Self {
            task: task.clone(),
            kind: ReviewKind::Combined {
                claims: claims.map(String::from),
                plan: plan.map(String::from),
                changed: changed.to_vec(),
                diff: diff.map(String::from),
            },
            pass,
            threshold,
        }
    }

    /// The multipass review's integration reviewer (pass 2) over the merged,
    /// de-duplicated per-file findings.
    pub(crate) fn integration(
        task: &Task,
        findings_json: &str,
        changed: &[String],
        pass: u32,
        threshold: f64,
    ) -> Self {
        Self {
            task: task.clone(),
            kind: ReviewKind::Integration {
                findings_json: findings_json.into(),
                changed: changed.to_vec(),
            },
            pass,
            threshold,
        }
    }
}

impl Agent for ReviewerAgent {
    type Session = ReviewOutcome;

    fn role(&self) -> AgentRole {
        AgentRole::Reviewer
    }

    fn system_prompt(&self) -> &'static str {
        REVIEWER_SYSTEM
    }

    fn user_prompt(&self) -> String {
        match &self.kind {
            ReviewKind::Combined {
                claims,
                plan,
                changed,
                diff,
            } => reviewer(
                &self.task,
                claims.as_deref(),
                plan.as_deref(),
                changed,
                diff.as_deref(),
                self.pass,
            ),
            ReviewKind::Integration {
                findings_json,
                changed,
            } => reviewer_integration(&self.task, findings_json, changed, self.pass),
        }
    }

    fn label(&self) -> String {
        "review".into()
    }

    fn tools(&self) -> &'static [Tool] {
        REVIEWER_TOOLS
    }

    fn validate(&self, events: &[AgentEvent]) -> Result<Self::Session, String> {
        let report = captured_text(events);
        let verdict = compute_verdict(Some(&report), self.threshold);
        Ok(ReviewOutcome { report, verdict })
    }
}

/// The multipass review's per-file reviewer: one file at a time, report-only
/// (the integration session decides what gets fixed), so its session's payload
/// is the parsed findings of its report.
pub(crate) struct FileReviewerAgent {
    task: Task,
    file: String,
    pass: u32,
}

impl FileReviewerAgent {
    pub(crate) fn new(task: &Task, file: &str, pass: u32) -> Self {
        Self {
            task: task.clone(),
            file: file.into(),
            pass,
        }
    }
}

impl Agent for FileReviewerAgent {
    type Session = Option<Findings>;

    fn role(&self) -> AgentRole {
        AgentRole::Reviewer
    }

    fn system_prompt(&self) -> &'static str {
        REVIEWER_SYSTEM
    }

    fn user_prompt(&self) -> String {
        reviewer_file(&self.task, &self.file, self.pass)
    }

    fn label(&self) -> String {
        "review".into()
    }

    fn tools(&self) -> &'static [Tool] {
        REVIEWER_TOOLS
    }

    fn validate(&self, events: &[AgentEvent]) -> Result<Self::Session, String> {
        Ok(parse_report(&captured_text(events)).findings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::test_task;

    #[test]
    fn generated_files_are_not_per_file_review_units() {
        use super::is_generated;
        assert!(is_generated("crates/x/tests/snapshots/ui__a.snap"));
        assert!(is_generated("Cargo.lock"));
        assert!(!is_generated("crates/x/src/ui.rs"));
    }

    #[test]
    fn the_reviewer_system_prompt_states_the_stage_rules() {
        // The reviewer framing.
        assert!(REVIEWER_SYSTEM.contains("Review and validate these claims. Find the gaps."));
        assert!(REVIEWER_SYSTEM.contains("fresh-context combined review-and-fix agent"));
        // The two passes: thorough validation, then assuming bugs remain.
        assert!(REVIEWER_SYSTEM.contains("First pass: thorough validation"));
        assert!(
            REVIEWER_SYSTEM.contains("Second pass: assume bugs remain and find what was missed")
        );
        // The tool surface and the no-modify rules.
        assert!(REVIEWER_SYSTEM.contains("run shell commands"));
        assert!(REVIEWER_SYSTEM.contains("Do not modify the spec file, the task list"));
    }

    #[test]
    fn the_reviewer_prompt_carries_the_claims_the_diff_and_the_rules() {
        let claims = "## Files Changed\nCREATE a.txt\n\n## Claims\n- [ ] it works";
        let p = reviewer(
            &test_task(),
            Some(claims),
            None,
            &["src/a.rs".into(), "src/b.rs".into()],
            Some("+++ b/src/a.rs\n+hello"),
            1,
        );
        // The task id and description.
        assert!(p.contains("Task T8.1: Add the per-task plan stage"));
        // The claims block, the diff and the pass number.
        assert!(p.contains(claims));
        assert!(p.contains("untrusted; verify every claim"));
        assert!(p.contains("```diff\n+++ b/src/a.rs\n+hello\n```"));
        assert!(p.contains("Pass 1."));
        // The spec and task file names.
        assert!(p.contains(SPEC_FILE) && p.contains(TASK_FILE));
        // The verify-independently, surgical-fix and no-separate-fix-loop rules.
        assert!(p.contains("never trust the builder's verification"));
        assert!(p.contains("independently"));
        assert!(p.contains("surgically, in the same pass"));
        assert!(p.contains("no separate fix loop"));
        assert!(p.contains("no surrounding refactors"));
        // The verdict line and the findings JSON contract.
        assert!(p.contains("Verdict: PASS"));
        assert!(p.contains("Verdict: FAIL"));
        assert!(p.contains("{\"high\": [], \"medium\": [], \"low\": []}"));
        assert!(p.contains("confidence between 0.0 and 1.0"));
        assert!(p.contains("whether you fixed it"));
        // The format rules are real newlines, not literal escape sequences.
        assert!(!p.contains("\\n"));
    }

    #[test]
    fn the_reviewer_prompt_falls_back_to_the_plan_and_the_file_list() {
        let p = reviewer(
            &test_task(),
            None,
            Some("## File Operations\nCREATE a.txt"),
            &["src/a.rs".into()],
            None,
            2,
        );
        assert!(p.contains("No build claims were found"));
        assert!(p.contains("## File Operations\nCREATE a.txt"));
        assert!(p.contains("- src/a.rs"));
        assert!(p.contains("The diff is empty or too large to carry"));
        assert!(p.contains("Pass 2."));
    }

    #[test]
    fn the_per_file_reviewer_prompt_is_report_only() {
        let p = reviewer_file(&test_task(), "src/a.rs", 1);
        assert!(p.contains("Task T8.1: Add the per-task plan stage"));
        assert!(p.contains("exactly this one: src/a.rs"));
        assert!(p.contains("single-file concerns only"));
        // Report-only: no edits, every finding fixed false.
        assert!(p.contains("Do not edit any file: this pass is report-only"));
        assert!(p.contains("\"fixed\": false"));
        assert!(p.contains("Verdict: PASS"));
        assert!(p.contains(SPEC_FILE) && p.contains(TASK_FILE));
        assert!(!p.contains("verify every claim"));
    }

    #[test]
    fn the_integration_prompt_carries_the_merged_findings() {
        let p = reviewer_integration(
            &test_task(),
            "{\"high\": [\"a leak\"], \"medium\": [], \"low\": []}",
            &["src/a.rs".into()],
            2,
        );
        assert!(p.contains("Task T8.1: Add the per-task plan stage"));
        assert!(p.contains("{\"high\": [\"a leak\"], \"medium\": [], \"low\": []}"));
        assert!(p.contains("Do not re-report those findings"));
        assert!(p.contains("cross-file issues"));
        assert!(p.contains("- src/a.rs"));
        // It reviews and fixes like the main pass: the shared rules travel along.
        assert!(p.contains("never trust the builder's verification"));
        assert!(p.contains("no separate fix loop"));
        assert!(p.contains("Verdict: PASS"));
    }

    #[test]
    fn the_verdict_line_parses_case_insensitively() {
        assert_eq!(parse_report("VERDICT: PASS").verdict_pass, Some(true));
        assert_eq!(parse_report("  verdict: fail").verdict_pass, Some(false));
        assert_eq!(parse_report("no verdict at all").verdict_pass, None);
        // The last verdict line wins.
        assert_eq!(
            parse_report("Verdict: PASS\nmore text\nverdict: FAIL").verdict_pass,
            Some(false)
        );
    }

    #[test]
    fn the_findings_block_parses_from_prose_or_a_fence() {
        let report = "Review prose.\n\n```json\n{\"high\": [{\"file\": \"a.rs\", \"issue\": \"leak\", \"confidence\": 0.9}], \"medium\": [], \"low\": []}\n```\nVerdict: FAIL";
        let parsed = parse_report(report);
        assert_eq!(parsed.verdict_pass, Some(false));
        let findings = parsed.findings.expect("valid block");
        assert_eq!(findings.high.len(), 1);
        assert_eq!(findings.high[0].file, "a.rs");
        assert_eq!(findings.high[0].confidence, 0.9);
    }

    #[test]
    fn an_empty_findings_block_is_valid() {
        let parsed = parse_report("Verdict: PASS\n{\"high\": [], \"medium\": [], \"low\": []}");
        assert_eq!(parsed.verdict_pass, Some(true));
        assert!(parsed.findings.is_some());
        // A JSON object without any severity key is not a findings block.
        assert!(
            parse_report("Verdict: FAIL\n{\"other\": 1}")
                .findings
                .is_none()
        );
        // Invalid JSON is not a findings block.
        assert!(
            parse_report("Verdict: FAIL\n{\"high\": [broken")
                .findings
                .is_none()
        );
    }

    #[test]
    fn an_empty_report_is_one_high_finding_and_fails() {
        let verdict = compute_verdict(Some("   "), 0.5);
        assert!(!verdict.passed);
        assert_eq!(verdict.high, 1);
        let verdict = compute_verdict(None, 0.5);
        assert!(!verdict.passed);
        assert_eq!(verdict.high, 1);
    }

    #[test]
    fn an_absent_findings_block_counts_as_one_high() {
        let verdict = compute_verdict(Some("Verdict: FAIL\nprose only"), 0.5);
        assert!(!verdict.passed);
        assert_eq!(verdict.high, 1);
        // A pass verdict carries it: verdict says PASS wins.
        let verdict = compute_verdict(Some("Verdict: PASS\nprose only"), 0.5);
        assert!(verdict.passed);
    }

    #[test]
    fn the_verdict_passes_on_zero_qualifying_highs_and_mediums() {
        let report = "Verdict: FAIL\n{\"high\": [], \"medium\": [], \"low\": [{\"issue\": \"style\", \"confidence\": 1.0}]}";
        assert!(compute_verdict(Some(report), 0.5).passed);
        // A pass verdict with a leftover qualifying finding still passes.
        let report = "Verdict: PASS\n{\"high\": [{\"issue\": \"x\", \"confidence\": 1.0}]}";
        assert!(compute_verdict(Some(report), 0.5).passed);
    }

    #[test]
    fn qualifying_highs_and_mediums_fail_the_review() {
        // A pass verdict with a leftover qualifying finding still passes (the
        // verdict line ORs with the findings), so the fail cases carry one.
        let report =
            "Verdict: FAIL\n{\"high\": [{\"issue\": \"x\", \"confidence\": 0.9}], \"medium\": []}";
        let verdict = compute_verdict(Some(report), 0.5);
        assert!(!verdict.passed);
        assert_eq!(verdict.high, 1);
        let report = "Verdict: FAIL\n{\"medium\": [{\"issue\": \"x\", \"confidence\": 0.9}]}";
        let verdict = compute_verdict(Some(report), 0.5);
        assert!(!verdict.passed);
        assert_eq!(verdict.medium, 1);
        // With no verdict line, the qualifying findings alone decide.
        let report = "{\"high\": [{\"issue\": \"x\", \"confidence\": 0.9}]}";
        let verdict = compute_verdict(Some(report), 0.5);
        assert!(!verdict.passed);
        assert_eq!(verdict.high, 1);
    }

    #[test]
    fn findings_below_the_threshold_do_not_count_and_are_returned() {
        let report = "Verdict: FAIL\n{\"high\": [{\"issue\": \"weak\", \"confidence\": 0.2}], \"medium\": [], \"low\": [{\"issue\": \"style\", \"confidence\": 1.0}]}";
        let verdict = compute_verdict(Some(report), 0.5);
        assert!(verdict.passed);
        assert_eq!(verdict.high, 0);
        assert_eq!(verdict.below_threshold.len(), 2);
        assert_eq!(verdict.below_threshold[0].severity, Severity::High);
        assert_eq!(verdict.below_threshold[1].severity, Severity::Low);
    }

    #[test]
    fn an_unknown_severity_key_is_dropped_as_before() {
        // A findings block naming a known bucket parses; the findings under
        // an unknown severity key are silently dropped by serde, exactly as
        // before the typed severity.
        let report = "Verdict: FAIL\n{\"high\": [], \"critical\": [{\"issue\": \"x\", \"confidence\": 1.0}]}";
        let parsed = parse_report(report);
        let findings = parsed.findings.expect("the high key makes it a block");
        assert!(findings.high.is_empty());
        assert!(findings.medium.is_empty());
        assert!(findings.low.is_empty());
        // With no qualifying finding, the verdict passes.
        assert!(compute_verdict(Some(report), 0.5).passed);
    }

    #[test]
    fn the_skip_rules_fire_in_precedence_order() {
        let base = SkipInputs {
            review_in_loop: true,
            batch_deferred: false,
            skip_review_for_simple: true,
            complexity: Some(Complexity::Simple),
            builder_clean: true,
            review_confidence_threshold: 0,
            history_consecutive_passes: None,
            stage_enabled: true,
        };
        // Review in loop off wins over everything.
        let mut inputs = base;
        inputs.review_in_loop = false;
        inputs.batch_deferred = true;
        assert_eq!(skip_review(&inputs), Some(SkipReason::ReviewInLoopOff));
        // Batch review beats the simple rule.
        let mut inputs = base;
        inputs.batch_deferred = true;
        assert_eq!(skip_review(&inputs), Some(SkipReason::BatchDeferred));
        // The simple rule fires for a clean simple build.
        assert_eq!(skip_review(&base), Some(SkipReason::SimpleWithPassingBuild));
        // A failed builder verification means the review runs.
        let mut inputs = base;
        inputs.builder_clean = false;
        assert_eq!(skip_review(&inputs), None);
        // Learned confidence applies to simple and medium shapes.
        let mut inputs = base;
        inputs.skip_review_for_simple = false;
        inputs.review_confidence_threshold = 5;
        inputs.history_consecutive_passes = Some(5);
        assert_eq!(skip_review(&inputs), Some(SkipReason::LearnedConfidence));
        inputs.history_consecutive_passes = Some(4);
        assert_eq!(skip_review(&inputs), None);
        // A threshold of 0 disables it.
        inputs.review_confidence_threshold = 0;
        inputs.history_consecutive_passes = Some(99);
        assert_eq!(skip_review(&inputs), None);
        // Complex tasks always review.
        let mut inputs = base;
        inputs.complexity = Some(Complexity::Complex);
        inputs.review_confidence_threshold = 5;
        inputs.history_consecutive_passes = Some(99);
        assert_eq!(skip_review(&inputs), None);
        // The stage disabled rule fires last.
        let mut inputs = base;
        inputs.skip_review_for_simple = false;
        inputs.stage_enabled = false;
        assert_eq!(skip_review(&inputs), Some(SkipReason::StageDisabled));
        // Every reason is recorded as a non-empty string.
        for reason in [
            SkipReason::ReviewInLoopOff,
            SkipReason::BatchDeferred,
            SkipReason::SimpleWithPassingBuild,
            SkipReason::LearnedConfidence,
            SkipReason::StageDisabled,
        ] {
            assert!(!reason.reason().is_empty());
        }
    }

    #[test]
    fn the_review_history_matches_and_records() {
        let mut history = ReviewHistory::default();
        // An empty history yields no skip.
        assert_eq!(
            history.match_shape("add the greeting file", Complexity::Simple),
            None
        );
        history.record(
            "add the greeting file",
            Complexity::Simple,
            true,
            "2026-01-01",
        );
        assert_eq!(
            history.match_shape("add the greeting file", Complexity::Simple),
            Some(1)
        );
        // A similar shape matches; a different one does not.
        assert_eq!(
            history.match_shape("add the greeting file again", Complexity::Simple),
            Some(1)
        );
        assert_eq!(
            history.match_shape("refactor the payment module", Complexity::Simple),
            None
        );
        // A fail resets the consecutive passes and stores the date.
        history.record(
            "add the greeting file",
            Complexity::Simple,
            false,
            "2026-01-02",
        );
        assert_eq!(
            history.match_shape("add the greeting file", Complexity::Simple),
            Some(0)
        );
        assert_eq!(history.clusters[0].last_fail.as_deref(), Some("2026-01-02"));
        // A pass increments again.
        history.record(
            "add the greeting file",
            Complexity::Simple,
            true,
            "2026-01-03",
        );
        assert_eq!(
            history.match_shape("add the greeting file", Complexity::Simple),
            Some(1)
        );
        // Complex tasks never match.
        assert_eq!(
            history.match_shape("add the greeting file", Complexity::Complex),
            None
        );
        // A medium shape is a separate cluster.
        history.record(
            "wire the review stage into the engine",
            Complexity::Medium,
            true,
            "2026-01-04",
        );
        assert_eq!(
            history.match_shape("wire the review stage into the engine", Complexity::Medium),
            Some(1)
        );
        assert_eq!(
            history.match_shape("wire the review stage into the engine", Complexity::Simple),
            None
        );
    }

    #[test]
    fn the_review_history_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("review-history.json");
        let mut history = ReviewHistory::default();
        history.record(
            "add the greeting file",
            Complexity::Simple,
            true,
            "2026-01-01",
        );
        history.save(&path).unwrap();
        assert_eq!(ReviewHistory::load(&path), history);
        // A missing file is an empty history.
        assert_eq!(
            ReviewHistory::load(&dir.path().join("missing.json")),
            ReviewHistory::default()
        );
    }

    #[test]
    fn the_cluster_cap_evicts_the_least_observed() {
        let mut history = ReviewHistory::default();
        // 200 mutually dissimilar shapes fill the history without matching.
        for n in 0..200 {
            history.record(
                &format!("alpha{n} beta{n} gamma{n} delta{n} epsilon{n}"),
                Complexity::Simple,
                true,
                "2026-01-01",
            );
        }
        assert_eq!(history.clusters.len(), 200);
        // A new shape still records: the least-observed cluster is evicted.
        history.record(
            "a brand new task shape entirely different",
            Complexity::Simple,
            true,
            "2026-01-01",
        );
        assert_eq!(history.clusters.len(), 200);
    }

    #[test]
    fn multipass_findings_merge_and_de_duplicate_by_file_and_issue() {
        let mut merged = Findings::default();
        let finding = |file: &str, issue: &str| Finding {
            file: file.into(),
            issue: issue.into(),
            confidence: 0.9,
            ..Finding::default()
        };
        for (file, issue) in [
            ("a.rs", "a leak"),
            ("a.rs", "a leak"),
            ("a.rs", "another"),
            ("b.rs", "a leak"),
        ] {
            let mut findings = Findings::default();
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
        let mut findings = Findings::default();
        findings.medium.push(finding("a.rs", "a leak"));
        merge_findings(&mut merged, findings);
        assert_eq!(merged.medium.len(), 1);
    }
}
