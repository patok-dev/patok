//! The `Provider` trait and one implementation per back-end.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use patok_core::config::ProviderKind;
use patok_core::event::AgentEvent;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

pub mod claude;
mod cli;
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
    /// The tools the session may use, provider-neutral; the provider maps each
    /// to its own CLI's tool name(s).
    pub allowed_tools: Vec<Tool>,
}

/// One tool an agent role's allowlist may name, provider-neutral: each provider
/// maps it to its own CLI's tool name(s).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Bash,
    Edit,
    Write,
    Read,
    Glob,
    Grep,
    NotebookEdit,
    WebFetch,
    WebSearch,
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

/// One back-end. Every provider exposes exactly one operation -- run a session --
/// plus the surface the engine needs to route and start it: the CLI it drives,
/// whether that CLI is installed, and whether it runs the configured reviewer
/// model.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Short provider identifier, e.g. `claude`.
    fn slug(&self) -> &'static str;

    /// Name of the CLI binary this provider drives, e.g. `claude`.
    fn cli_name(&self) -> &'static str;

    /// Whether the engine routes the configured reviewer model to sessions on
    /// this provider. Claude runs its reviewer with `reviewer_model`; the other
    /// providers run their CLI's default model (Codex accepts a `-m` flag, but
    /// reviewer model names are Claude model names that do not map to it).
    fn supports_reviewer_model(&self) -> bool;

    /// The availability check: `Err(CliNotFound)` when the CLI is not on PATH.
    fn check_available(&self) -> Result<(), ProviderError>;

    async fn run_session(&self, request: SessionRequest) -> Result<SessionResult, ProviderError>;
}

/// The provider for `kind`, after checking that its CLI is on `PATH`.
pub fn resolve(kind: ProviderKind) -> Result<std::sync::Arc<dyn Provider>, ProviderError> {
    let provider: std::sync::Arc<dyn Provider> = match kind {
        ProviderKind::Claude => std::sync::Arc::new(claude::Claude),
        ProviderKind::Mistral => std::sync::Arc::new(vibe::Vibe),
        ProviderKind::Codex => std::sync::Arc::new(codex::Codex),
    };
    provider.check_available()?;
    Ok(provider)
}

/// The Builder role's tool allowlist.
pub const BUILDER_TOOLS: &[Tool] = &[
    Tool::Bash,
    Tool::Edit,
    Tool::Write,
    Tool::Read,
    Tool::Glob,
    Tool::Grep,
    Tool::NotebookEdit,
    Tool::WebFetch,
    Tool::WebSearch,
];

/// The Planner role's tool allowlist: read-only exploration plus file writes, no shell or web.
pub const PLANNER_TOOLS: &[Tool] = &[Tool::Read, Tool::Glob, Tool::Grep, Tool::Edit, Tool::Write];

/// The Discovery role's tool allowlist: read-only exploration, no edits, writes, shell or web.
pub const DISCOVERY_TOOLS: &[Tool] = &[Tool::Read, Tool::Glob, Tool::Grep];

/// The Reviewer role's tool allowlist: read and search
/// the project, run shell commands, edit and write files -- the combined
/// review-and-fix agent of the review stage.
pub const REVIEWER_TOOLS: &[Tool] = &[
    Tool::Read,
    Tool::Glob,
    Tool::Grep,
    Tool::Bash,
    Tool::Edit,
    Tool::Write,
];

/// The Plan role's tool allowlist: read-only exploration, no edits, writes, shell or web.
pub const PLAN_TOOLS: &[Tool] = &[Tool::Read, Tool::Glob, Tool::Grep];

/// The Research role's tool allowlist: read and search
/// the project, run shell commands and look things up on the web.
pub const RESEARCH_TOOLS: &[Tool] = &[
    Tool::Read,
    Tool::Glob,
    Tool::Grep,
    Tool::Bash,
    Tool::Write,
    Tool::WebFetch,
    Tool::WebSearch,
];

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
            &[
                Tool::Read,
                Tool::Glob,
                Tool::Grep,
                Tool::Bash,
                Tool::Edit,
                Tool::Write
            ]
        );
    }

    /// Only the claude provider runs its reviewer with the configured reviewer
    /// model; the other providers and the default mock keep their CLI's default.
    #[test]
    fn only_claude_supports_the_reviewer_model() {
        assert!(claude::Claude.supports_reviewer_model());
        assert!(!codex::Codex.supports_reviewer_model());
        assert!(!vibe::Vibe.supports_reviewer_model());
        assert!(!mock::MockProvider::new(vec![]).supports_reviewer_model());
        assert!(
            mock::MockProvider::new(vec![])
                .with_reviewer_model(true)
                .supports_reviewer_model()
        );
    }

    /// The CLI names the engine and the availability check report per provider.
    #[test]
    fn each_provider_names_its_cli() {
        assert_eq!(claude::Claude.cli_name(), "claude");
        assert_eq!(codex::Codex.cli_name(), "codex");
        assert_eq!(vibe::Vibe.cli_name(), "vibe");
        assert_eq!(mock::MockProvider::new(vec![]).cli_name(), "mock");
        // A scripted provider drives no CLI, so it is always available.
        assert!(mock::MockProvider::new(vec![]).check_available().is_ok());
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
