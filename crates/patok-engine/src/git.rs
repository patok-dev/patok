//! Git operations through the `git` CLI.
//! Explicit argument vectors, never a shell string; user config, hooks and signing apply.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

const FOOTER: &str = "Automated by: patok";
const SUBJECT_LIMIT: usize = 72;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitKind {
    /// Validated work.
    Feat,
    /// Work that did not validate, committed to preserve progress.
    Wip,
}

pub struct Git {
    dir: PathBuf,
}

impl Git {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    async fn run(&self, args: &[&str]) -> Option<std::process::Output> {
        Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| tracing::warn!("git {args:?} could not run: {e}"))
            .ok()
    }

    async fn succeeds(&self, args: &[&str]) -> bool {
        self.run(args).await.is_some_and(|o| o.status.success())
    }

    pub async fn is_repo(&self) -> bool {
        self.succeeds(&["rev-parse", "--is-inside-work-tree"]).await
    }

    /// The current HEAD's short SHA, or `None` outside a repository.
    pub async fn head_sha(&self) -> Option<String> {
        let out = self.run(&["rev-parse", "--short", "HEAD"]).await?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|sha| !sha.is_empty())
    }

    /// The changed files of the working tree: the `git status --porcelain` names, plus, when `base` is
    /// given, the names that differ from it -- so a batch review's changed set
    /// spans every commit of the group. Returns an empty vector outside a
    /// repository.
    pub async fn changed_files(&self, base: Option<&str>) -> Vec<String> {
        let mut files = Vec::new();
        if let Some(out) = self.run(&["status", "--porcelain"]).await {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(name) = porcelain_name(line) {
                    files.push(name);
                }
            }
        }
        if let Some(base) = base {
            let args = ["diff", "--name-only", base];
            if let Some(out) = self.run(&args).await {
                for line in String::from_utf8_lossy(&out.stdout).lines() {
                    let name = line.trim().to_string();
                    if !name.is_empty() {
                        files.push(name);
                    }
                }
            }
        }
        files.sort();
        files.dedup();
        files
    }

    /// The diff of the work under review: against `base` when given (a batch
    /// review spans every commit of its group), otherwise `HEAD`. Empty outside
    /// a repository; untracked files do not appear in a diff.
    pub async fn diff(&self, base: Option<&str>) -> String {
        let target = base.unwrap_or("HEAD");
        match self.run(&["diff", "--no-color", target]).await {
            Some(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
            None => String::new(),
        }
    }

    /// Stages everything and commits. Returns the short SHA, or `None` when nothing was
    /// committed: not a repository, nothing staged, or the commit failed. All three are
    /// normal outcomes, not errors (Part I, 9.1).
    pub async fn commit_all(
        &self,
        kind: CommitKind,
        task_id: &str,
        description: &str,
    ) -> Option<String> {
        if !self.is_repo().await {
            return None;
        }
        if !self
            .succeeds(&["rev-parse", "--verify", "-q", "HEAD"])
            .await
            && !self
                .succeeds(&[
                    "commit",
                    "--allow-empty",
                    "-m",
                    "chore: initial commit",
                    "-m",
                    FOOTER,
                ])
                .await
        {
            return None;
        }
        if !self.succeeds(&["add", "-A"]).await {
            return None;
        }
        // Exit status 1 means there are staged changes.
        if self.succeeds(&["diff", "--cached", "--quiet"]).await {
            return None;
        }
        let (label, body) = match kind {
            CommitKind::Feat => (
                "feat",
                "Implemented and validated by autonomous build loop.",
            ),
            CommitKind::Wip => (
                "WIP",
                "Validation did not pass. Committing to preserve progress.",
            ),
        };
        let description: String = description.chars().take(SUBJECT_LIMIT).collect();
        let subject = format!("{label}({task_id}): {description}");
        if !self
            .succeeds(&["commit", "-m", &subject, "-m", body, "-m", FOOTER])
            .await
        {
            return None;
        }
        let out = self.run(&["rev-parse", "--short", "HEAD"]).await?;
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

/// The file name of one `git status --porcelain` line: the name after the
/// two-column status, taking the new side of a rename (`R  old -> new`).
fn porcelain_name(line: &str) -> Option<String> {
    let name = line.get(3..)?;
    let name = name.rsplit(" -> ").next().unwrap_or(name);
    let name = name.trim_matches('"');
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub async fn init_repo(dir: &Path) {
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "Test"],
            &["config", "user.email", "test@example.com"],
            &["config", "commit.gpgsign", "false"],
        ] {
            assert!(Git::new(dir).succeeds(args).await);
        }
    }

    #[tokio::test]
    async fn commits_with_convention_and_adds_initial_commit() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let git = Git::new(dir.path());
        let sha = git
            .commit_all(CommitKind::Feat, "T1.1", "do the thing")
            .await
            .unwrap();
        assert!(!sha.is_empty());
        let log = git.run(&["log", "--format=%s%n%b---", "-2"]).await.unwrap();
        let log = String::from_utf8_lossy(&log.stdout).to_string();
        assert!(log.contains("feat(T1.1): do the thing"), "{log}");
        assert!(log.contains("Implemented and validated by autonomous build loop."));
        assert!(log.contains("Automated by: patok"));
        assert!(log.contains("chore: initial commit"));
    }

    #[tokio::test]
    async fn wip_subject_is_truncated_and_nothing_staged_is_not_committed() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let git = Git::new(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let long = "x".repeat(100);
        assert!(
            git.commit_all(CommitKind::Wip, "T1.2", &long)
                .await
                .is_some()
        );
        let subject = git.run(&["log", "-1", "--format=%s"]).await.unwrap();
        let subject = String::from_utf8_lossy(&subject.stdout).trim().to_string();
        assert_eq!(subject, format!("WIP(T1.2): {}", "x".repeat(72)));
        assert!(
            git.commit_all(CommitKind::Feat, "T1.3", "nothing changed")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn head_sha_reads_the_current_commit() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        let git = Git::new(dir.path());
        assert!(git.head_sha().await.is_none(), "no commit yet");
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        git.commit_all(CommitKind::Feat, "T1.1", "first").await;
        let sha = git.head_sha().await.unwrap();
        assert!(!sha.is_empty());
    }

    #[tokio::test]
    async fn changed_files_list_working_tree_and_base_names() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let git = Git::new(dir.path());
        git.commit_all(CommitKind::Feat, "T1.1", "initial").await;
        // Uncommitted changes: a modified tracked file and a new untracked one.
        std::fs::write(dir.path().join("a.txt"), "changed").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b").unwrap();
        assert_eq!(
            git.changed_files(None).await,
            vec!["a.txt".to_string(), "b.txt".to_string()]
        );
        // A base spanning several commits: the diff names join the set.
        let base = git.head_sha().await.unwrap();
        std::fs::write(dir.path().join("c.txt"), "c").unwrap();
        git.commit_all(CommitKind::Feat, "T1.2", "second").await;
        std::fs::write(dir.path().join("d.txt"), "d").unwrap();
        git.commit_all(CommitKind::Feat, "T1.3", "third").await;
        let files = git.changed_files(Some(&base)).await;
        assert!(files.contains(&"c.txt".to_string()), "{files:?}");
        assert!(files.contains(&"d.txt".to_string()), "{files:?}");
        assert!(files.contains(&"a.txt".to_string()), "{files:?}");
        // A clean tree against HEAD is empty.
        git.commit_all(CommitKind::Feat, "T1.4", "fourth").await;
        assert!(git.changed_files(None).await.is_empty());
    }

    #[tokio::test]
    async fn diff_spans_a_base_and_the_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path()).await;
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let git = Git::new(dir.path());
        git.commit_all(CommitKind::Feat, "T1.1", "initial").await;
        let base = git.head_sha().await.unwrap();
        // A committed change since the base appears in the base diff.
        std::fs::write(dir.path().join("b.txt"), "committed later").unwrap();
        git.commit_all(CommitKind::Feat, "T1.2", "second").await;
        // An uncommitted change appears against both bases.
        std::fs::write(dir.path().join("c.txt"), "working tree").unwrap();
        let diff = git.diff(Some(&base)).await;
        assert!(diff.contains("b.txt"), "{diff}");
        let diff = git.diff(None).await;
        assert!(
            !diff.contains("b.txt"),
            "HEAD diff holds only uncommitted work"
        );
    }

    #[tokio::test]
    async fn non_repo_has_no_changed_files_or_diff() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let git = Git::new(dir.path());
        assert!(git.changed_files(None).await.is_empty());
        assert!(git.diff(None).await.is_empty());
        assert_eq!(
            porcelain_name("?? \"quoted name.txt\""),
            Some("quoted name.txt".into())
        );
        assert_eq!(
            porcelain_name("R  old.txt -> new.txt"),
            Some("new.txt".into())
        );
        assert_eq!(
            porcelain_name(" M modified.txt"),
            Some("modified.txt".into())
        );
    }

    #[tokio::test]
    async fn non_repo_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        assert!(
            Git::new(dir.path())
                .commit_all(CommitKind::Feat, "T1.1", "x")
                .await
                .is_none()
        );
    }
}
