//! The review stage's pure logic:
//! the claims trimming rule, the review report's verdict and findings parsing,
//! the verdict computation, the skip decision in precedence order, the
//! five-position progress indicator, the persistent learned-confidence
//! history and the persistent open-group record with its resume decision.
//! Everything here is deterministic and unit-tested; the engine wiring in
//! `engine.rs` calls into it.

use std::collections::HashSet;
use std::path::Path;

use patok_core::complexity::Complexity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Findings above this many lines in the verification-results section of the
/// claims trigger the trim.
const VERIFICATION_TRIM_LINES: usize = 100;
/// The head and tail the trimmed verification section keeps.
const TRIM_HEAD: usize = 20;
const TRIM_TAIL: usize = 10;
/// Substrings that keep a trimmed middle line in the section.
const TRIM_KEEP: [&str; 4] = ["failed", "error", "panic", "assert"];
/// The keywords of the findings buckets.
const SEVERITIES: [&str; 3] = ["high", "medium", "low"];

/// The claims file's build-claims section, from its heading to the end of the
/// text (`None` when the builder's final message has no such heading). The
/// heading matches case-insensitively at any level.
pub fn claims_section(text: &str) -> Option<&str> {
    let start = text.lines().find_map(|line| {
        heading_is(line, "Build Claims").then(|| line.as_ptr() as usize - text.as_ptr() as usize)
    });
    start.map(|start| &text[start..])
}

/// A heading line (`#` through `######`) whose name equals `name`
/// case-insensitively.
fn heading_is(line: &str, name: &str) -> bool {
    let Some(after) = line.trim_start().strip_prefix('#') else {
        return false;
    };
    let after = after.trim_start_matches('#').trim();
    !after.is_empty() && after.eq_ignore_ascii_case(name)
}

/// Trims an over-long verification-results section of the build claims: a section over 100 lines is reduced to its first 20
/// lines, a "trimmed N lines" marker, any middle lines mentioning failed,
/// error, panic or assert (case-insensitively), and its last 10 lines.
/// Everything else in the claims is untouched.
pub fn trim_claims(claims: &str) -> String {
    let Some(bounds) = section_bounds(claims, "Verification Results") else {
        return claims.to_string();
    };
    // The section's content: every line after its heading, up to the next one.
    let content: Vec<&str> = claims[bounds.0..bounds.1].lines().skip(1).collect();
    if content.len() <= VERIFICATION_TRIM_LINES {
        return claims.to_string();
    }
    let heading_end = bounds.0
        + claims[bounds.0..bounds.1]
            .lines()
            .next()
            .map_or(0, str::len)
        + 1;
    let mut trimmed = String::with_capacity(claims.len());
    trimmed.push_str(&claims[..heading_end]);
    for line in &content[..TRIM_HEAD] {
        trimmed.push_str(line);
        trimmed.push('\n');
    }
    let middle = &content[TRIM_HEAD..content.len() - TRIM_TAIL];
    let kept: Vec<&&str> = middle
        .iter()
        .filter(|line| {
            let lower = line.to_lowercase();
            TRIM_KEEP.iter().any(|needle| lower.contains(needle))
        })
        .collect();
    trimmed.push_str(&format!(
        "[trimmed {} lines of verification output]\n",
        middle.len() - kept.len()
    ));
    for line in kept {
        trimmed.push_str(line);
        trimmed.push('\n');
    }
    for line in &content[content.len() - TRIM_TAIL..] {
        trimmed.push_str(line);
        trimmed.push('\n');
    }
    trimmed.push_str(&claims[bounds.1..]);
    trimmed
}

/// The byte bounds of the first section named `name` in `text`: the start of
/// its heading line and the start of the next heading line (or the end).
fn section_bounds(text: &str, name: &str) -> Option<(usize, usize)> {
    let mut start = None;
    for line in text.lines() {
        let offset = line.as_ptr() as usize - text.as_ptr() as usize;
        if !line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(start) = start {
            return Some((start, offset));
        }
        if heading_is(line, name) {
            start = Some(offset);
        }
    }
    start.map(|start| (start, text.len()))
}

/// One finding of the review report's JSON block. Every field is optional at the JSON level -- the
/// reviewer is an LLM -- but the severity buckets are positional.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Finding {
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
pub struct Findings {
    #[serde(default)]
    pub high: Vec<Finding>,
    #[serde(default)]
    pub medium: Vec<Finding>,
    #[serde(default)]
    pub low: Vec<Finding>,
}

impl Findings {
    /// The findings of `severity` ("high", "medium" or "low").
    pub fn bucket(&self, severity: &str) -> &[Finding] {
        match severity {
            "high" => &self.high,
            "medium" => &self.medium,
            _ => &self.low,
        }
    }
}

/// A parsed review report: the case-insensitive `Verdict: PASS` line, when
/// present, and the findings JSON block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReportParse {
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
pub fn parse_report(text: &str) -> ReportParse {
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
            SEVERITIES
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
pub struct QualifyingFinding {
    pub severity: &'static str,
    pub finding: Finding,
}

/// The computed verdict of one review.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
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
pub fn compute_verdict(report: Option<&str>, threshold: f64) -> Verdict {
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
    for severity in ["high", "medium"] {
        for finding in findings.bucket(severity) {
            let counted = QualifyingFinding {
                severity,
                finding: finding.clone(),
            };
            if finding.confidence >= threshold {
                verdict.qualifying.push(counted);
                if severity == "high" {
                    verdict.high += 1;
                } else {
                    verdict.medium += 1;
                }
            } else {
                verdict.below_threshold.push(counted);
            }
        }
    }
    for finding in findings.bucket("low") {
        verdict.below_threshold.push(QualifyingFinding {
            severity: "low",
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
pub fn is_generated(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    name.ends_with(".snap")
        || name.ends_with(".snap.new")
        || name.ends_with(".lock")
        || name == "package-lock.json"
}

/// The single HIGH finding an absent or invalid findings block counts as.
fn missing_report() -> QualifyingFinding {
    QualifyingFinding {
        severity: "high",
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
pub enum SkipReason {
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
    pub fn reason(self) -> &'static str {
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
pub struct SkipInputs {
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
pub fn skip_review(inputs: &SkipInputs) -> Option<SkipReason> {
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

/// The leading number of a task ID (`T1.2` -> `Some("1")`): the digits before
/// the dot, shared by a batch-review group. `None` for malformed IDs.
pub fn leading_number(task_id: &str) -> Option<String> {
    let (digits, _) = task_id.rsplit_once('.')?;
    let stripped = digits
        .strip_prefix(|c: char| c.is_alphabetic())
        .unwrap_or(digits);
    (!stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit()))
        .then(|| stripped.to_string())
}

/// Whether a pending task after the current one shares its leading number, so
/// batch review defers the current task's review to the group's last task. The
/// current task's own ID never counts.
pub fn batch_deferred(current: &str, pending_after: &[String]) -> bool {
    let Some(number) = leading_number(current) else {
        return false;
    };
    pending_after
        .iter()
        .any(|id| id != current && leading_number(id).as_deref() == Some(number.as_str()))
}

/// The five-position progress indicator written into the task line: Research, Plan, a
/// plan-self-review sub-slot, Build, Review -- each the stage letters when it
/// ran or `-` when skipped, the sub-slot `.` in this build -- followed by `!`
/// when the task was not validated.
pub fn progress(
    ran_research: bool,
    ran_plan: bool,
    ran_build: bool,
    ran_review: bool,
    validated: bool,
) -> String {
    let mut token = String::from("[");
    token.push(if ran_research { 'R' } else { '-' });
    token.push(if ran_plan { 'P' } else { '-' });
    token.push('.');
    token.push(if ran_build { 'B' } else { '-' });
    token.push(if ran_review { 'R' } else { '-' });
    if !validated {
        token.push('!');
    }
    token.push(']');
    token
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
pub struct ReviewHistory {
    clusters: Vec<Cluster>,
}

impl ReviewHistory {
    /// Loads the history from `path`; a missing file is an empty history.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Saves the history to `path`, best-effort: the error is returned for the
    /// caller to log.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string(self).unwrap_or_default())
    }

    /// The consecutive review passes of the best-matching cluster for the task
    /// shape: the same tier (Simple or Medium only) and Jaccard word overlap
    /// at or above the matching similarity. `None` when nothing matches.
    pub fn match_shape(&self, description: &str, tier: Complexity) -> Option<u32> {
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
    pub fn record(&mut self, description: &str, tier: Complexity, passed: bool, date: &str) {
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

/// The batch-review group currently open across restarts: the shared leading
/// task-ID number, the group's base and last full commit SHAs, and the
/// (task ID, description hash) of every task committed into the group.
/// Stored as `open-group.json` in the data dir; a missing or corrupt file
/// means no open group.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OpenGroup {
    /// The group's leading task-ID number (`"1"` for `T1.2`).
    pub number: String,
    /// The full SHA of the commit the group started at.
    pub base: String,
    /// The full SHA of the group's last commit.
    pub last: String,
    /// `(task_id, description_hash)` per task committed into the group.
    pub members: Vec<(String, String)>,
}

impl OpenGroup {
    /// Loads the open group from `path`; a missing or corrupt file is no
    /// open group.
    pub fn load(path: &Path) -> Option<Self> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
    }

    /// Saves the open group to `path`, best-effort: the error is returned
    /// for the caller to log.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_string(self).unwrap_or_default())
    }

    /// Clears the open-group record by deleting its file; a missing file is
    /// already clear.
    pub fn clear(path: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// The SHA-256 hex digest of a task description: the description's identity
/// for group membership, stable across restarts.
pub fn description_hash(description: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(description.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether an interrupted batch-review group resumes or starts fresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeDecision {
    /// Reuse the open group's base so the review still diffs every commit
    /// of the group.
    Resume,
    /// Start a fresh group at the current HEAD.
    Fresh,
}

/// Decides whether the open group resumes: the task's leading number matches
/// the record, `head` equals the record's `last`, the record's `base` is an
/// ancestor of HEAD, and the task is a member (its (id, description hash) is
/// in `members`) or a not-yet-run task of the same number (its ID is absent
/// from `members`). Anything else -- a moved HEAD, a non-ancestor base, a
/// different number, or a reused ID whose description hash differs -- starts
/// a fresh group.
pub fn resume_decision(
    record: &OpenGroup,
    head: &str,
    base_is_ancestor: bool,
    task_id: &str,
    description: &str,
) -> ResumeDecision {
    let number_matches = leading_number(task_id).is_some_and(|number| number == record.number);
    let hash = description_hash(description);
    let is_member = record
        .members
        .iter()
        .any(|(id, member_hash)| id == task_id && member_hash == &hash);
    let is_unrun = !record.members.iter().any(|(id, _)| id == task_id);
    if number_matches && head == record.last && base_is_ancestor && (is_member || is_unrun) {
        ResumeDecision::Resume
    } else {
        ResumeDecision::Fresh
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

#[cfg(test)]
mod tests {
    #[test]
    fn generated_files_are_not_per_file_review_units() {
        use super::is_generated;
        assert!(is_generated("crates/x/tests/snapshots/ui__a.snap"));
        assert!(is_generated("Cargo.lock"));
        assert!(!is_generated("crates/x/src/ui.rs"));
    }

    use super::*;

    #[test]
    fn claims_section_starts_at_the_heading() {
        let text = "Summary first.\n\n## Build Claims\n## Files Changed\nCREATE a\n";
        assert_eq!(
            claims_section(text),
            Some("## Build Claims\n## Files Changed\nCREATE a\n")
        );
        assert_eq!(claims_section("no headings"), None);
        // Case-insensitive at any level.
        assert!(claims_section("#### build claims\nx").is_some());
    }

    #[test]
    fn a_short_verification_section_is_kept_whole() {
        let claims = "## Verification Results\n1. cargo test\n2. cargo clippy\n";
        assert_eq!(trim_claims(claims), claims);
        // A claims without the section is untouched.
        assert_eq!(
            trim_claims("## Files Changed\nCREATE a\n"),
            "## Files Changed\nCREATE a\n"
        );
    }

    #[test]
    fn an_over_long_verification_section_is_trimmed() {
        let mut section = String::from("## Verification Results\n");
        for i in 0..120 {
            section.push_str(&format!("line {i} of the output\n"));
        }
        section.push_str("## Claims\n- [ ] it works\n");
        let trimmed = trim_claims(&section);
        // The head and tail survive.
        assert!(trimmed.contains("line 0 of the output"));
        assert!(trimmed.contains("line 19 of the output"));
        assert!(trimmed.contains("line 119 of the output"));
        assert!(trimmed.contains("line 110 of the output"));
        // The middle is gone except the marker and the kept lines.
        assert!(trimmed.contains("[trimmed 90 lines of verification output]"));
        assert!(!trimmed.contains("line 50 of the output"));
        // The sections around the trimmed one are untouched.
        assert!(trimmed.contains("## Claims\n- [ ] it works"));
        assert!(trimmed.starts_with("## Verification Results\n"));
        assert_eq!(trimmed.matches("## Verification Results").count(), 1);
    }

    #[test]
    fn middle_lines_mentioning_failures_survive_the_trim() {
        let mut section = String::from("## Verification Results\n");
        for i in 0..110 {
            if i == 50 {
                section.push_str("error: assertion failed in the middle\n");
            } else {
                section.push_str(&format!("line {i}\n"));
            }
        }
        let trimmed = trim_claims(&section);
        assert!(trimmed.contains("error: assertion failed in the middle"));
        assert!(trimmed.contains("[trimmed 79 lines of verification output]"));
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
        // A pass verdict with leftover qualifying findings still passes.
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
        assert_eq!(verdict.below_threshold[0].severity, "high");
        assert_eq!(verdict.below_threshold[1].severity, "low");
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
    fn leading_numbers_and_batch_deferral() {
        assert_eq!(leading_number("T1.2"), Some("1".into()));
        assert_eq!(leading_number("D12.3"), Some("12".into()));
        assert_eq!(leading_number("1.2"), Some("1".into()));
        assert_eq!(leading_number("T1"), None);
        assert_eq!(leading_number("TASK"), None);
        assert!(batch_deferred("T1.1", &["T1.2".into(), "T1.3".into()]));
        assert!(!batch_deferred("T1.1", &["T2.1".into()]));
        // The current task never defers itself.
        assert!(!batch_deferred("T1.1", &["T1.1".into()]));
        // A single-task group is never deferred.
        assert!(!batch_deferred("T1.1", &["T2.1".into(), "T3.1".into()]));
    }

    #[test]
    fn the_progress_token_has_five_positions_and_the_exclamation() {
        assert_eq!(progress(true, true, true, true, true), "[RP.BR]");
        assert_eq!(progress(false, false, true, false, true), "[--.B-]");
        assert_eq!(progress(true, true, true, true, false), "[RP.BR!]");
        assert_eq!(progress(false, false, true, false, false), "[--.B-!]");
        assert_eq!(progress(true, true, false, false, false), "[RP.--!]");
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
    fn the_open_group_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("open-group.json");
        let group = OpenGroup {
            number: "1".into(),
            base: "b1a20c1d0e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b".into(),
            last: "7f6e5d4c3b2a1f0e9d8c7b6a5f4e3d2c1b0a9f8e".into(),
            members: vec![
                ("T1.1".into(), description_hash("first task description")),
                ("T1.2".into(), "0123456789abcdef0123456789abcdef".into()),
            ],
        };
        group.save(&path).unwrap();
        assert_eq!(OpenGroup::load(&path), Some(group));
        // A missing file (a closed group, a fresh install) is no open group.
        assert_eq!(OpenGroup::load(&dir.path().join("missing.json")), None);
        // A corrupt file is no open group either.
        std::fs::write(dir.path().join("corrupt.json"), "not json").unwrap();
        assert_eq!(OpenGroup::load(&dir.path().join("corrupt.json")), None);
        // Clearing deletes the record; clearing again is already clear.
        OpenGroup::clear(&path).unwrap();
        assert_eq!(OpenGroup::load(&path), None);
        assert!(OpenGroup::clear(&path).is_ok());
        // The description hash is deterministic and distinguishes descriptions.
        assert_eq!(description_hash("a task"), description_hash("a task"));
        assert_ne!(description_hash("a"), description_hash("b"));
    }

    #[test]
    fn an_interrupted_group_resumes_for_a_member_task() {
        let description = "resume the interrupted group";
        let record = OpenGroup {
            number: "1".into(),
            base: "base".into(),
            last: "c0ffee".into(),
            members: vec![("T1.2".into(), description_hash(description))],
        };
        assert_eq!(
            resume_decision(&record, "c0ffee", true, "T1.2", description),
            ResumeDecision::Resume
        );
    }

    #[test]
    fn a_moved_head_starts_a_fresh_group() {
        let description = "resume the interrupted group";
        let record = OpenGroup {
            number: "1".into(),
            base: "base".into(),
            last: "c0ffee".into(),
            members: vec![("T1.2".into(), description_hash(description))],
        };
        assert_eq!(
            resume_decision(&record, "head moved on", true, "T1.2", description),
            ResumeDecision::Fresh
        );
    }

    #[test]
    fn a_base_that_is_not_an_ancestor_starts_a_fresh_group() {
        let description = "resume the interrupted group";
        let record = OpenGroup {
            number: "1".into(),
            base: "abandoned".into(),
            last: "c0ffee".into(),
            members: vec![("T1.2".into(), description_hash(description))],
        };
        assert_eq!(
            resume_decision(&record, "c0ffee", false, "T1.2", description),
            ResumeDecision::Fresh
        );
    }

    #[test]
    fn a_different_leading_number_starts_a_fresh_group() {
        let description = "a task of another number";
        let record = OpenGroup {
            number: "2".into(),
            base: "base".into(),
            last: "c0ffee".into(),
            members: vec![("T1.2".into(), description_hash(description))],
        };
        assert_eq!(
            resume_decision(&record, "c0ffee", true, "T1.2", description),
            ResumeDecision::Fresh
        );
        // A malformed task ID has no leading number, so it starts fresh too.
        assert_eq!(
            resume_decision(&record, "c0ffee", true, "no-dot", description),
            ResumeDecision::Fresh
        );
    }

    #[test]
    fn a_reused_task_id_with_a_new_description_starts_a_fresh_group() {
        let record = OpenGroup {
            number: "1".into(),
            base: "base".into(),
            last: "c0ffee".into(),
            members: vec![("T1.2".into(), description_hash("old description"))],
        };
        assert_eq!(
            resume_decision(&record, "c0ffee", true, "T1.2", "a reworded description"),
            ResumeDecision::Fresh
        );
    }

    #[test]
    fn a_not_yet_run_task_of_the_same_number_resumes() {
        let record = OpenGroup {
            number: "1".into(),
            base: "base".into(),
            last: "c0ffee".into(),
            members: vec![("T1.1".into(), "0123456789abcdef0123456789abcdef".into())],
        };
        assert_eq!(
            resume_decision(&record, "c0ffee", true, "T1.2", "the next task"),
            ResumeDecision::Resume
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
}
