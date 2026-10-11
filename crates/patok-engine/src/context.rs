use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use patok_core::config::DaemonSettings;
use patok_core::event::{AgentRole, EngineEvent, NoticeLevel, SessionOutcome};
use patok_core::pipeline::Stage;
use patok_providers::Provider;
use tokio_util::sync::CancellationToken;

use crate::Engine;
use crate::engine::{Inner, Tiles, resolve_provider};
use crate::git::Git;
use crate::task_list::TaskList;

/// One session's context (section 8 of docs/private/engine-restructure.md):
/// the unit-of-work snapshot taken at the session's start -- the provider,
/// the model and the merged settings it relies on -- plus the session's
/// cancel token and a private handle on the engine internals. The steps and
/// pipelines borrow `&SessionContext`; it is the only bridge to the engine's
/// bus, tile and reconcile paths, exposed through these narrow methods.
#[derive(Clone)]
pub(crate) struct SessionContext {
    inner: Arc<Inner>,
    provider: Arc<dyn Provider>,
    model: Option<String>,
    settings: DaemonSettings,
    cancel: CancellationToken,
}

impl SessionContext {
    pub(crate) fn new(
        inner: Arc<Inner>,
        provider: Arc<dyn Provider>,
        model: Option<String>,
        settings: DaemonSettings,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            inner,
            provider,
            model,
            settings,
            cancel,
        }
    }

    /// The facade over this context's internals handle, for delegation to the
    /// engine's existing bus, tile and reconcile paths.
    fn engine(&self) -> Engine {
        Engine {
            inner: self.inner.clone(),
        }
    }

    /// The provider the session's runs use, unless a run names its own (the
    /// review stage's reviewer provider).
    pub(crate) fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    /// The session's model override, if one is configured.
    pub(crate) fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The session's settings snapshot, taken when it started.
    pub(crate) fn settings(&self) -> &DaemonSettings {
        &self.settings
    }

    /// The idle timeout of one agent session (`agent_timeout_secs`).
    pub(crate) fn agent_timeout(&self) -> Duration {
        Duration::from_secs(self.settings.agent_timeout_secs)
    }

    /// Whether the review stage is enabled in the pipeline stage list (an
    /// absent entry is disabled).
    pub(crate) fn review_stage_enabled(&self) -> bool {
        self.settings
            .stages
            .iter()
            .find(|entry| entry.stage == Stage::Review)
            .is_some_and(|entry| entry.enabled)
    }

    /// The token that cancels the session's running agent.
    pub(crate) fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Records the event for replay and pushes it to the attached shell.
    pub(crate) fn emit(&self, event: EngineEvent) {
        self.engine().emit(event);
    }

    /// Emits one notice to the attached shell.
    pub(crate) fn notice(&self, level: NoticeLevel, text: impl Into<String>) {
        self.engine().notice(level, text);
    }

    /// Announces `role`'s agent as the engine's active agent and returns the
    /// instant the session's duration is measured from.
    pub(crate) fn announce_agent(
        &self,
        role: AgentRole,
        provider: &str,
        model: Option<&str>,
    ) -> Instant {
        self.engine().announce_session(role, provider, model)
    }

    /// Pushes one agent session's end to the attached shells and the replayed
    /// recent events.
    pub(crate) fn finish_agent(&self, role: AgentRole, start: Instant, outcome: SessionOutcome) {
        self.engine().finish_session(role, start, outcome);
    }

    /// One stage-tile update followed by the rail refresh -- the single
    /// update path every tile change goes through.
    pub(crate) fn update_tiles(&self, update: impl FnOnce(&mut Tiles)) {
        self.engine().update_tiles(update);
    }

    /// The engine's task list: the task-file path, the in-memory queue and
    /// the file lock guarding it.
    pub(crate) fn task_list(&self) -> &TaskList {
        &self.inner.task_list
    }

    /// Re-reads the task file, merges it into the queue and broadcasts the
    /// new list when it changed; returns whether it did.
    pub(crate) fn reconcile(&self) -> bool {
        self.engine().reconcile()
    }

    /// A git handle on the session's project directory.
    pub(crate) fn git(&self) -> Git {
        Git::new(&self.inner.config.project_dir)
    }

    /// The project directory the session builds in.
    pub(crate) fn project_dir(&self) -> &Path {
        &self.inner.config.project_dir
    }

    /// The session's persistent per-project data directory.
    pub(crate) fn data_dir(&self) -> &Path {
        &self.inner.config.data_dir
    }

    /// The directory holding the agents' history logs.
    pub(crate) fn history_dir(&self) -> PathBuf {
        self.data_dir().join("history")
    }

    /// The persistent learned-confidence history file.
    pub(crate) fn review_history_path(&self) -> &Path {
        &self.inner.config.review_history
    }

    /// The open-group record (`open-group.json`): the batch-review group open
    /// across restarts.
    pub(crate) fn open_group_path(&self) -> &Path {
        &self.inner.config.open_group
    }

    /// The leading task-ID number of the current batch-review group.
    pub(crate) fn group_number(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .expect("engine state lock")
            .group_number
            .clone()
    }

    /// The full SHA the current batch-review group diffs across.
    pub(crate) fn group_base(&self) -> Option<String> {
        self.inner
            .state
            .lock()
            .expect("engine state lock")
            .group_base
            .clone()
    }

    /// Writes the batch-review group's number and base in one state update.
    pub(crate) fn set_group(&self, number: Option<String>, base: Option<String>) {
        let mut state = self.inner.state.lock().expect("engine state lock");
        state.group_number = number;
        state.group_base = base;
    }

    /// The reviewer session's provider and model when the reviewer provider
    /// names a provider this build implements (`reviewer_provider`): that
    /// provider, resolved like any other unit of work. A provider that
    /// supports the reviewer model (the claude provider) runs with the
    /// configured one (`reviewer_model`); the other providers keep their
    /// default model. A name this build does not implement keeps the
    /// configured pair silently; an unavailable provider warns and falls back
    /// to the configured pair, as does a configured reviewer provider on a
    /// fixed-provider engine without a resolver.
    pub(crate) fn reviewer(&self) -> (Arc<dyn Provider>, Option<String>) {
        let configured = (self.provider.clone(), self.model.clone());
        let Some(resolver) = self.inner.resolver.as_ref() else {
            if self.settings.reviewer_provider.kind().is_some() {
                let name = &self.settings.reviewer_provider;
                tracing::warn!(
                    "the {name} reviewer provider has no resolver; \
                     the reviewer runs on the fixed provider"
                );
            }
            return configured;
        };
        match resolve_provider(resolver, &self.settings.reviewer_provider) {
            Ok(Some(provider)) => {
                let model = provider
                    .supports_reviewer_model()
                    .then(|| self.settings.reviewer_model.clone());
                (provider, model)
            }
            // A name this build does not implement: keep the configured
            // provider silently, exactly like today.
            Ok(None) => configured,
            Err(error) => {
                let name = &self.settings.reviewer_provider;
                tracing::warn!(
                    "the {name} reviewer provider is unavailable ({error}); \
                     the reviewer runs on the configured provider"
                );
                configured
            }
        }
    }
}
