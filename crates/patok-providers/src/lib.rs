//! The `Provider` trait and one implementation per back-end.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use patok_core::config::ProviderKind;
use patok_core::event::AgentEvent;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

pub mod claude;
pub mod codex;
pub mod mock;
mod process;
pub mod vibe;

/// Everything a provider needs to run one agent session.
pub struct SessionRequest {
    /// Model name; `None` lets the CLI pick its default.
    pub model: Option<String>,
    pub prompt: String,
    /// Appended to the CLI's own system prompt, not replacing it.
    pub system_prompt: Option<String>,
    /// Working directory of the agent.
    pub project_dir: PathBuf,
    /// Normalised events are pushed here as the session streams.
    pub events: UnboundedSender<AgentEvent>,
    /// Directory for the raw transcript; created if missing.
    pub log_dir: PathBuf,
    /// Short label (usually the task ID) used in the transcript file name.
    pub label: String,
    /// Kill the agent after this long without progress; the hard limit is four times this.
    pub idle_timeout: Duration,
    pub cancel: CancellationToken,
    pub allowed_tools: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

/// Outcome of a session. Process-level failures after a successful spawn are reported here,
/// not as errors.
#[derive(Clone, Debug)]
pub struct SessionResult {
    pub success: bool,
    pub exit: ExitKind,
    pub failure: Option<String>,
    pub log_path: Option<PathBuf>,
    /// The provider and model actually used.
    pub provider: String,
    pub model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("the `{cli}` CLI was not found on PATH; install it or change the provider setting")]
    CliNotFound { cli: String },
    #[error("failed to start `{cli}`: {source}")]
    Spawn { cli: String, source: std::io::Error },
    #[error("cannot write the session transcript in {}: {source}", dir.display())]
    Log {
        dir: PathBuf,
        source: std::io::Error,
    },
}

/// One back-end. Every provider exposes exactly one operation: run a session.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Short provider identifier, e.g. `claude`.
    fn slug(&self) -> &'static str;

    async fn run_session(&self, request: SessionRequest) -> Result<SessionResult, ProviderError>;
}

/// The provider for `kind`, after checking that its CLI is on `PATH`.
pub fn resolve(kind: ProviderKind) -> Result<std::sync::Arc<dyn Provider>, ProviderError> {
    match kind {
        ProviderKind::Claude => {
            if !cli_on_path("claude") {
                return Err(ProviderError::CliNotFound {
                    cli: "claude".into(),
                });
            }
            Ok(std::sync::Arc::new(claude::Claude))
        }
        ProviderKind::Mistral => {
            vibe::Vibe::check_available()?;
            Ok(std::sync::Arc::new(vibe::Vibe))
        }
        ProviderKind::Codex => {
            codex::Codex::check_available()?;
            Ok(std::sync::Arc::new(codex::Codex))
        }
    }
}

/// The Builder role's tool allowlist.
pub const BUILDER_TOOLS: &[&str] = &[
    "Bash",
    "Edit",
    "Write",
    "Read",
    "Glob",
    "Grep",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
];

/// The Planner role's tool allowlist: read-only exploration plus file writes, no shell or web.
pub const PLANNER_TOOLS: &[&str] = &["Read", "Glob", "Grep", "Edit", "Write"];

/// The Discovery role's tool allowlist: read-only exploration, no edits, writes, shell or web.
pub const DISCOVERY_TOOLS: &[&str] = &["Read", "Glob", "Grep"];

/// The Reviewer role's tool allowlist: read and search
/// the project, run shell commands, edit and write files -- the combined
/// review-and-fix agent of the review stage.
pub const REVIEWER_TOOLS: &[&str] = &["Read", "Glob", "Grep", "Bash", "Edit", "Write"];

/// The Plan role's tool allowlist: read-only exploration, no edits, writes, shell or web.
pub const PLAN_TOOLS: &[&str] = &["Read", "Glob", "Grep"];

/// The Research role's tool allowlist: read and search
/// the project, run shell commands and look things up on the web.
pub const RESEARCH_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "Bash",
    "Write",
    "WebFetch",
    "WebSearch",
];

/// Whether an executable called `name` is on `PATH` (the provider availability check).
pub fn cli_on_path(name: &str) -> bool {
    cli_in(name, std::env::var_os("PATH").as_deref())
}

/// Whether an executable called `name` is in one of the directories of `path`.
pub fn cli_in(name: &str, path: Option<&std::ffi::OsStr>) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_some_and(|path| {
        std::env::split_paths(path).any(|dir| {
            std::fs::metadata(dir.join(name))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reviewer_allowlist_is_the_spec_reviewer_row() {
        assert_eq!(
            REVIEWER_TOOLS,
            &["Read", "Glob", "Grep", "Bash", "Edit", "Write"]
        );
    }

    /// Whichever side of the availability check this machine lands on, the error or the
    /// provider must name codex.
    #[test]
    fn resolve_codex_names_the_codex_cli() {
        match resolve(ProviderKind::Codex) {
            Ok(provider) => assert_eq!(provider.slug(), "codex"),
            Err(ProviderError::CliNotFound { cli }) => assert_eq!(cli, "codex"),
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
