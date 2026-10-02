//! A scripted provider for engine and pipeline tests.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use patok_core::event::AgentEvent;

use crate::{ExitKind, Provider, ProviderError, SessionRequest, SessionResult};

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
    pub allowed_tools: Vec<String>,
}

#[derive(Clone)]
pub struct MockProvider {
    /// One script per session; sessions past the list replay the last script.
    scripts: Vec<Vec<Step>>,
    exit: ExitKind,
    /// Overrides the provider slug, so tests can tell mock instances routed
    /// for different provider kinds apart (`None`: "mock").
    slug: Option<&'static str>,
    sessions: Arc<Mutex<Vec<RecordedSession>>>,
}

impl MockProvider {
    /// A session that plays `steps` and then completes successfully.
    pub fn new(steps: Vec<Step>) -> Self {
        Self {
            scripts: vec![steps],
            exit: ExitKind::Completed,
            slug: None,
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

    pub fn sessions(&self) -> Vec<RecordedSession> {
        self.sessions.lock().expect("mock lock").clone()
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn slug(&self) -> &'static str {
        self.slug.unwrap_or("mock")
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
