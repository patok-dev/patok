//! A scripted provider for engine and pipeline tests.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use patok_core::event::AgentEvent;
use serde_json;

use crate::{
    DiscoveredModel, ExitKind, Provider, ProviderError, SessionRequest, SessionResult, Tool,
};

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
    /// Whether `supports_model_selection()` reports true, so tests can script
    /// either answer; defaults to false, like Vibe.
    supports_model_selection: bool,
    /// Whether every session reports a rate limit.
    rate_limited: bool,
    /// What `discover_models()` returns (`None`: the default unsupported error).
    discovered_models: Option<Vec<DiscoveredModel>>,
    sessions: Arc<Mutex<Vec<RecordedSession>>>,
}

impl MockProvider {
    /// A session that plays `steps` and then completes successfully.
    pub fn new(steps: Vec<Step>) -> Self {
        Self {
            scripts: vec![steps],
            exit: ExitKind::Completed,
            slug: None,
            supports_model_selection: false,
            rate_limited: false,
            discovered_models: None,
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
            supports_model_selection: false,
            rate_limited: false,
            discovered_models: None,
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

    /// Reports `on` from `supports_model_selection()`, so tests can script a
    /// provider that accepts a model choice (like the claude provider) or not.
    pub fn with_model_selection(mut self, on: bool) -> Self {
        self.supports_model_selection = on;
        self
    }

    /// Reports every session as rate limited, so tests can drive the full (non-adaptive) pause.
    pub fn with_rate_limited(mut self, on: bool) -> Self {
        self.rate_limited = on;
        self
    }

    /// Reports `models` from `discover_models()`, so tests can script a provider
    /// whose harness lists models.
    pub fn with_discovered_models(mut self, models: Vec<DiscoveredModel>) -> Self {
        self.discovered_models = Some(models);
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

    fn supports_model_selection(&self) -> bool {
        self.supports_model_selection
    }

    /// A scripted provider drives no CLI, so it is always available.
    fn check_available(&self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn discover_models(&self) -> Result<Vec<DiscoveredModel>, ProviderError> {
        match &self.discovered_models {
            Some(models) => Ok(models.clone()),
            None => Err(ProviderError::DiscoveryUnsupported {
                provider: self.slug().into(),
            }),
        }
    }

    async fn run_session(&self, request: SessionRequest) -> Result<SessionResult, ProviderError> {
        // Extract the label (agent name) from the raw_log_path filename
        // Format: <seq>-<agent>-<provider>-raw.jsonl
        let filename = request
            .raw_log_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let label = if let Some(base) = std::path::Path::new(&filename).file_stem() {
            let name = base.to_string_lossy().into_owned();
            // Extract the agent part from "<seq>-<agent>-<provider>-raw"
            if let Some(pos) = name.rfind("-raw") {
                let prefix = &name[..pos];
                if let Some(pos2) = prefix.rfind('-') {
                    let prefix2 = &prefix[..pos2];
                    if let Some(pos3) = prefix2.rfind('-') {
                        prefix2[pos3 + 1..].to_string()
                    } else {
                        prefix2.to_string()
                    }
                } else {
                    name
                }
            } else {
                name
            }
        } else {
            filename
        };

        let index = {
            let mut sessions = self.sessions.lock().expect("mock lock");
            sessions.push(RecordedSession {
                prompt: request.prompt.clone(),
                system_prompt: request.system_prompt.clone(),
                model: request.model.clone(),
                label,
                allowed_tools: request.allowed_tools.clone(),
            });
            sessions.len() - 1
        };
        let log_path = request.raw_log_path.clone();
        let mut transcript = String::new();

        // Create parent directory if it doesn't exist
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ProviderError::Log {
                path: log_path.clone(),
                source,
            })?;
        }

        let script = &self.scripts[std::cmp::min(index, self.scripts.len() - 1)];
        for step in script {
            match step {
                Step::Event(event) => {
                    // Write each event as a JSONL line
                    let json_line = serde_json::to_string(event).unwrap_or_else(|_| "{}".into());
                    transcript.push_str(&format!("{json_line}\n"));
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
                ExitKind::Failed => Some(crate::process::agent_exited_unsuccessfully("mock")),
                ExitKind::Cancelled => Some(crate::process::CANCELLED_BY_SHUTDOWN.into()),
                // A scripted timeout has no real duration to report, so the mock keeps
                // the short form of the timeout message.
                ExitKind::TimedOut => Some("timed out".into()),
            },
            log_path: Some(log_path),
            provider: "mock".into(),
            model: String::new(),
            rate_limited: self.rate_limited,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scripted provider defaults to the non-claude answers -- no model
    /// selection, no CLI to check -- and `with_model_selection` flips the capability.
    #[test]
    fn the_capability_defaults_off_and_flips_with_the_builder() {
        let mock = MockProvider::new(vec![]);
        assert!(!mock.supports_model_selection());
        assert!(mock.check_available().is_ok());
        assert_eq!(mock.slug(), "mock");
        let claude_like = mock.clone().with_slug("claude").with_model_selection(true);
        assert!(claude_like.supports_model_selection());
        assert_eq!(claude_like.slug(), "claude");
        // The flag is per instance: the default mock keeps its answer.
        assert!(!mock.supports_model_selection());
    }

    #[tokio::test]
    async fn discovery_defaults_to_unsupported_and_is_scriptable() {
        let mock = MockProvider::new(vec![]);
        match mock.discover_models().await {
            Err(ProviderError::DiscoveryUnsupported { provider }) => assert_eq!(provider, "mock"),
            other => panic!("unexpected result: {other:?}"),
        }
        let models = vec![DiscoveredModel {
            id: "m1".into(),
            display_name: None,
            listed: true,
        }];
        let scripted = mock
            .with_slug("claude")
            .with_discovered_models(models.clone());
        assert_eq!(scripted.discover_models().await.unwrap(), models);
    }
}
