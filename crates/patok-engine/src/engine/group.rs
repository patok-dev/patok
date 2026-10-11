//! The batch-review group tracking and the task-line machinery: the leading
//! task-ID number, the batch deferral, the five-position progress indicator
//! and the persistent open-group record with its resume decision. Everything
//! here is deterministic and unit-tested; the group wiring in `commit` calls
//! into it.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The leading number of a task ID (`T1.2` -> `Some("1")`): the digits before
/// the dot, shared by a batch-review group. `None` for malformed IDs.
pub(crate) fn leading_number(task_id: &str) -> Option<String> {
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
pub(crate) fn batch_deferred(current: &str, pending_after: &[String]) -> bool {
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
pub(crate) fn progress(
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

/// The batch-review group currently open across restarts: the shared leading
/// task-ID number, the group's base and last full commit SHAs, and the
/// (task ID, description hash) of every task committed into the group.
/// Stored as `open-group.json` in the data dir; a missing or corrupt file
/// means no open group.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct OpenGroup {
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
    pub(crate) fn load(path: &Path) -> Option<Self> {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
    }

    /// Saves the open group to `path`, best-effort: the error is returned
    /// for the caller to log.
    pub(crate) fn save(&self, path: &Path) -> std::io::Result<()> {
        patok_core::utils::atomic_write::write_atomic(
            path,
            &serde_json::to_string(self).unwrap_or_default(),
        )
    }

    /// Clears the open-group record by deleting its file; a missing file is
    /// already clear.
    pub(crate) fn clear(path: &Path) -> std::io::Result<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

/// The SHA-256 hex digest of a task description: the description's identity
/// for group membership, stable across restarts.
pub(crate) fn description_hash(description: &str) -> String {
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
pub(crate) enum ResumeDecision {
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
pub(crate) fn resume_decision(
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
