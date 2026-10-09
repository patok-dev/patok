//! A scripted provider for engine and pipeline tests.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use patok_core::event::AgentEvent;

use crate::{ExitKind, Provider, ProviderError, SessionRequest, SessionResult, Tool};

/// One step of a scripted session.
#[derive(Clone, Debug)]
pub enum Step {
    Event(AgentEvent),
    /// Writes a file relative to the project directory, as an agent editing code would.
    WriteFile {
        path: PathBuf,
        contents: String,
    },
    Sleep(Duration),
}

/// What the engine asked the mock to do, for assertions.
#[derive(Clone, Debug)]
pub struct RecordedSession {
    pub prompt: String,
    pub system_prompt: Option<String>,
    /// The model the engine routed the session to (`None`: the provider picks).
    pub model: Option<String>,
    pub label: String,
    /// The provider-neutral tools the engine allowed the session.
    pub allowed_tools: Vec<Tool>,
}

#[derive(Clone)]
pub struct MockProvider {
    /// One script per session; sessions past the list replay the last script.
    scripts: Vec<Vec<Step>>,
    exit: ExitKind,
    /// Overrides the provider slug, so tests can tell mock instances routed
    /// for different provider kinds apart (`None`: "mock").
    slug: Option<&'static str>,
    /// Whether `supports_reviewer_model()` reports true, so tests can script
    /// either answer; defaults to false like the non-claude providers.
    supports_reviewer_model: bool,
    sessions: Arc<Mutex<Vec<RecordedSession>>>,
}

impl MockProvider {
    /// A session that plays `steps` and then completes successfully.
    pub fn new(steps: Vec<Step>) -> Self {
        Self {
            scripts: vec![steps],
            exit: ExitKind::Completed,
            slug: None,
            supports_reviewer_model: false,
            sessions: Arc::default(),
        }
    }

    /// A provider that plays a different script per session: the nth session plays
    /// `scripts[n]`, and sessions past the list replay the last one. Each session ends the
    /// same way as [`MockProvider::new`] (or `exit` via [`MockProvider::ending_with`]).
    pub fn per_session(scripts: Vec<Vec<Step>>) -> Self {
        assert!(!scripts.is_empty(), "per_session needs at least one script");
        Self {
            scripts,
            exit: ExitKind::Completed,
            slug: None,
            supports_reviewer_model: false,
            sessions: Arc::default(),
        }
    }

    /// Ends the scripted session with `exit` instead of completing.
    pub fn ending_with(mut self, exit: ExitKind) -> Self {
        self.exit = exit;
        self
    }

    /// Reports `slug` from `slug()` instead of "mock", keeping the shared
    /// session record, so tests can assert provider routing by kind.
    pub fn with_slug(mut self, slug: &'static str) -> Self {
        self.slug = Some(slug);
        self
    }

    /// Reports `on` from `supports_reviewer_model()`, so tests can script a
    /// provider that runs the reviewer with the configured model (like the
    /// claude provider) or without it.
    pub fn with_reviewer_model(mut self, on: bool) -> Self {
        self.supports_reviewer_model = on;
        self
    }

    pub fn sessions(&self) -> Vec<RecordedSession> {
        self.sessions.lock().expect("mock lock").clone()
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn slug(&self) -> &'static str {
        self.slug.unwrap_or("mock")
    }

    fn cli_name(&self) -> &'static str {
        "mock"
    }

    fn supports_reviewer_model(&self) -> bool {
        self.supports_reviewer_model
    }

    /// A scripted provider drives no CLI, so it is always available.
    fn check_available(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn run_session(&self, request: SessionRequest) -> Result<SessionResult, ProviderError> {
        let index = {
            let mut sessions = self.sessions.lock().expect("mock lock");
            sessions.push(RecordedSession {
                prompt: request.prompt.clone(),
                system_prompt: request.system_prompt.clone(),
                model: request.model.clone(),
                label: request.label.clone(),
                allowed_tools: request.allowed_tools.clone(),
            });
            sessions.len() - 1
        };
        let log_path = crate::process::open_log_path(&request.log_dir, &request.label, "mock")?;
        let mut transcript = String::new();

        let script = &self.scripts[std::cmp::min(index, self.scripts.len() - 1)];
        for step in script {
            match step {
                Step::Event(event) => {
                    transcript.push_str(&format!("{event:?}\n"));
                    let _ = request.events.send(event.clone());
                }
                Step::WriteFile { path, contents } => {
                    let target = request.project_dir.join(path);
                    if let Some(parent) = target.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(target, contents);
                }
                Step::Sleep(duration) => {
                    tokio::select! {
                        () = tokio::time::sleep(*duration) => {}
                        () = request.cancel.cancelled() => {
                            return Ok(self.finish(log_path, &transcript, ExitKind::Cancelled));
                        }
                    }
                }
            }
        }
        Ok(self.finish(log_path, &transcript, self.exit))
    }
}

impl MockProvider {
    fn finish(&self, log_path: PathBuf, transcript: &str, exit: ExitKind) -> SessionResult {
        let _ = std::fs::write(&log_path, transcript);
        SessionResult {
            success: exit == ExitKind::Completed,
            exit,
            failure: match exit {
                ExitKind::Completed => None,
                ExitKind::Failed => Some("Agent mock exited unsuccessfully".into()),
                ExitKind::Cancelled => Some("cancelled by shutdown".into()),
                ExitKind::TimedOut => Some("timed out".into()),
            },
            log_path: Some(log_path),
            provider: "mock".into(),
            model: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scripted provider defaults to the non-claude answers -- no reviewer
    /// model, no CLI to check -- and `with_reviewer_model` flips the capability.
    #[test]
    fn the_capability_defaults_off_and_flips_with_the_builder() {
        let mock = MockProvider::new(vec![]);
        assert!(!mock.supports_reviewer_model());
        assert!(mock.check_available().is_ok());
        assert_eq!(mock.cli_name(), "mock");
        assert_eq!(mock.slug(), "mock");
        let claude_like = mock.clone().with_slug("claude").with_reviewer_model(true);
        assert!(claude_like.supports_reviewer_model());
        assert_eq!(claude_like.slug(), "claude");
        // The flag is per instance: the default mock keeps its answer.
        assert!(!mock.supports_reviewer_model());
    }
}
