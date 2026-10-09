use std::path::PathBuf;
use std::time::Duration;

use patok_core::config::{
    ApplyTiming, DaemonSettings, SettingValue, daemon_apply_timing, daemon_changed_fields,
    daemon_field_value, load_daemon, set_daemon_field,
};
use patok_core::event::EngineEvent;
use patok_core::pipeline::Stage;

use super::Engine;
use super::state::Unit;

/// How often the config files' modification times are checked.
const CONFIG_FILE_POLL: Duration = Duration::from_secs(2);

impl Engine {
    /// The central in-memory daemon settings: every consumer
    /// reads through this rather than holding an independent copy.
    pub fn settings(&self) -> DaemonSettings {
        self.central().settings.clone()
    }

    /// The unit-of-work snapshot the starting commands validate and run with: the fixed
    /// provider on `Engine::new` engines, else the central settings re-resolved through
    /// the resolver. Errors are user-facing rejection reasons, exactly like a startup
    /// resolution failure.
    pub(crate) fn unit(&self) -> Result<Unit, String> {
        let central = self.central();
        let provider = match self.inner.resolver.as_ref() {
            Some(resolver) => {
                // The startup load's hard errors (an unrecognized provider value) refuse
                // runs; a healthy config re-resolves from the current settings.
                if let Err(error) = self.inner.provider.as_ref() {
                    return Err(error.clone());
                }
                let name = central.settings.provider.to_string();
                match central.settings.provider.kind() {
                    Some(kind) => resolver(kind).map_err(|error| {
                        tracing::error!("provider {name} unavailable: {error}");
                        error.to_string()
                    })?,
                    None => {
                        return Err(format!(
                            "the `{name}` provider is not available in this build; use claude, codex or mistral"
                        ));
                    }
                }
            }
            None => self.inner.provider.clone()?,
        };
        Ok(Unit {
            provider,
            model: central.model.clone(),
            agent_timeout: Duration::from_secs(central.settings.agent_timeout_secs),
            plan_enabled: central.settings.plan_enabled,
            run_mode: central.settings.run_mode,
            skip_research_for_simple: central.settings.skip_research_for_simple,
            review_in_loop: central.settings.review_in_loop,
            skip_review_for_simple: central.settings.skip_review_for_simple,
            batch_review: central.settings.batch_review,
            review_confidence_threshold: central.settings.review_confidence_threshold,
            review_multipass_threshold: central.settings.review_multipass_threshold,
            confidence_threshold: central.settings.confidence_threshold,
            reviewer_provider: central.settings.reviewer_provider,
            reviewer_model: central.settings.reviewer_model.clone(),
            review_stage_enabled: central
                .settings
                .stages
                .iter()
                .find(|entry| entry.stage == Stage::Review)
                .is_some_and(|entry| entry.enabled),
        })
    }

    /// Applies one settings-overlay change to a daemon-schema field: validated exactly as an on-disk field, persisted to the resolved layer
    /// (`patok.config.toml` in the project's slot of the projects dir), re-merged into
    /// the central config and broadcast back field
    /// by field. Returns when the change takes effect; errors are user-facing rejection
    /// reasons.
    pub fn apply_settings_change(
        &self,
        field: &str,
        value: SettingValue,
    ) -> Result<ApplyTiming, String> {
        let mut warnings = Vec::new();
        set_daemon_field(&self.inner.config.data_dir, field, value, &mut warnings)
            .map_err(|error| error.message)?;
        for warning in &warnings {
            tracing::warn!("{warning}");
        }
        let timing = daemon_apply_timing(field).unwrap_or(ApplyTiming::NextUnitOfWork);
        self.reload_config();
        Ok(timing)
    }

    /// Re-merges the config files and the environment into the central settings and
    /// swaps the result in: the same merge and normalization as at startup, field by
    /// field, with the same fallback warnings. Used after the engine persists a change
    /// and when a file changes on disk.
    fn reload_config(&self) {
        let loaded = load_daemon(
            &self.inner.config.config_files,
            &self.inner.config.project_dir,
            &self.inner.config.data_dir,
            &self.inner.env,
        );
        self.swap_settings(loaded.settings, &loaded.warnings);
    }

    /// Swaps `new` in as the central settings: logs the reload `warnings`, refreshes the
    /// display fields, reconfigures the discovery schedule and broadcasts one
    /// `ConfigChanged` per changed field, carrying the field's effective (post-merge,
    /// post-normalization) value and apply timing. An empty
    /// diff emits nothing.
    fn swap_settings(&self, new: DaemonSettings, warnings: &[String]) {
        for warning in warnings {
            tracing::warn!("{warning}");
        }
        let changed = {
            let mut central = self.central();
            let old = std::mem::replace(&mut central.settings, new.clone());
            if self.inner.resolver.is_some() {
                central.provider_name = new.provider.to_string();
                central.model = new.model.clone();
            }
            daemon_changed_fields(&old, &new)
        };
        let mut state = self.state();
        state.schedule.reconfigure(
            Duration::from_secs(new.discovery_cooldown_secs),
            Duration::from_secs(new.discovery_cooldown_cap_secs),
        );
        // A sprint <-> continuous change re-mutes or unmutes the DISCOVER tile
        // live, so the rail follows the central mode; the running loop still
        // honors the mode from its session snapshot and picks the change up at
        // the next build-loop start.
        let run_mode_changed = changed.iter().any(|field| field == "run_mode");
        for field in changed {
            let Some(value) = daemon_field_value(&new, &field) else {
                continue;
            };
            let timing = daemon_apply_timing(&field).unwrap_or(ApplyTiming::NextUnitOfWork);
            self.emit_locked(
                &mut state,
                EngineEvent::ConfigChanged {
                    field,
                    value,
                    timing,
                },
            );
        }
        if run_mode_changed {
            self.refresh_pipeline_locked(&mut state);
        }
    }

    /// Polls the config files' modification times and reloads the central settings when
    /// one changes. Runs until the engine
    /// shuts down.
    pub fn spawn_config_watch(&self) {
        let engine = self.clone();
        tokio::spawn(async move {
            let paths: Vec<PathBuf> = [
                engine.inner.config.config_files.user_global.clone(),
                engine.inner.config.config_files.user_local.clone(),
                Some(patok_core::config::project_file(
                    &engine.inner.config.data_dir,
                )),
            ]
            .into_iter()
            .flatten()
            .collect();
            let mtimes = || -> Vec<_> {
                paths
                    .iter()
                    .map(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok())
                    .collect()
            };
            let mut last = mtimes();
            let mut tick = tokio::time::interval(CONFIG_FILE_POLL);
            loop {
                tokio::select! {
                    () = engine.inner.shutdown.cancelled() => return,
                    _ = tick.tick() => {}
                }
                let now = mtimes();
                if now != last {
                    last = now;
                    engine.reload_config();
                }
            }
        });
    }
}
