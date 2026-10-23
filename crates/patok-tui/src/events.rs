//! The event reducer (finding 19, the rest, in
//! docs/private/structure-review-followup.md): the one place engine events
//! fold into the shell -- the run state (the phase and run flags, the active
//! agent, provider and model, the task list) and the output pane's lines,
//! mapped by the shared [`event_lines`], plus the reducer's own focus and
//! session-timer helpers, split out of `app` so the state types and the
//! reducer each read as one thing. Pure: no I/O, so it is tested without a
//! terminal.

use patok_core::event::{AgentEvent, EngineEvent, Phase, TaskOutcome};
use patok_core::output::{LineKind, LineOrigin, OutLine, event_lines};

use crate::app::App;
use crate::modal::Modal;
use crate::view::FrameFocus;

impl App {
    pub fn apply(&mut self, event: EngineEvent) {
        // The shared mapping owns the pane's lines; it reads only the pane's
        // open flag, captured here before the state match runs.
        let out = event_lines(&event, self.run.output.is_open());
        match event {
            EngineEvent::PhaseChanged { phase } => {
                let was_idle = self.is_idle();
                if phase == Phase::Running {
                    self.run.new_tasks.clear();
                }
                self.run.phase = phase;
                // Leaving the idle state points the view at the run's output.
                self.sync_focus(was_idle);
                self.sync_session(was_idle);
            }
            EngineEvent::TaskStarted { id, .. } => {
                self.run.current_task = Some(id);
            }
            EngineEvent::Agent { event } => {
                // Usage feeds the run state's readout, not the pane; every
                // other agent event is pure line mapping, applied below.
                if let AgentEvent::Usage(usage) = event
                    && !self.run.planning
                {
                    self.run.usage = Some(usage);
                }
            }
            // Replayed with the recent events, so a reattaching shell shows the right one.
            // The engine-recorded start anchors the running timer, so it and the
            // finished line derive from the same session timing (T42.1). The
            // engine announces the run's agent before the run's flag flips
            // (T24.1), so the anchor must not be gated on the busy
            // classification: `AgentChanged` lands while the shell still
            // classifies itself as idle, and the timer's visibility stays
            // handled by `session_elapsed`'s `is_idle` gate (T131.1). A
            // `started_ms` of 0 means unknown and is skipped.
            EngineEvent::AgentChanged { agent, started_ms } => {
                self.run.agent = agent;
                if started_ms > 0 {
                    let start = self.clock.anchored(started_ms);
                    self.clock.session_start = Some(start);
                    // Only the run's first agent anchors the overall timer.
                    self.clock.overall_start.get_or_insert(start);
                }
            }
            // The session's provider and model drive the output frame title
            // while it runs, so a reviewer on another provider (a `claude`
            // reviewer provider) is titled accordingly instead of silently
            // reusing the configured one (T81.1).
            EngineEvent::AgentStarted {
                provider, model, ..
            } => {
                self.run.provider = provider;
                self.run.model = model.unwrap_or_default();
            }
            // The frame title returns to the configured provider and model
            // (T81.1).
            EngineEvent::AgentFinished { .. } => {
                self.run.provider = self.run.config_provider.clone();
                self.run.model = self.run.config_model.clone();
            }
            // The running task (the pane's marker) is tracked by ID, so a replaced list keeps it.
            EngineEvent::TasksChanged { tasks } => {
                for task in &tasks {
                    if !self.run.tasks.iter().any(|t| t.id == task.id) {
                        self.run.new_tasks.push(task.id.clone());
                    }
                }
                self.run.tasks = tasks;
            }
            EngineEvent::PlanningChanged { planning } => {
                let was_idle = self.is_idle();
                // A status message set during the run (a busy refusal)
                // clears with it; the run's own outcome is the notice line
                // in the output pane, not the status bar (T29.1).
                if !planning && self.run.planning {
                    self.status = None;
                }
                self.run.planning = planning;
                self.sync_focus(was_idle);
                self.sync_session(was_idle);
            }
            EngineEvent::DiscoveryChanged { discovering } => {
                let was_idle = self.is_idle();
                self.run.discovering = discovering;
                self.sync_focus(was_idle);
                self.sync_session(was_idle);
            }
            EngineEvent::TaskFinished { outcome, .. } => {
                self.run.current_task = None;
                if matches!(outcome, TaskOutcome::Done | TaskOutcome::Finished) {
                    self.run.completed += 1;
                }
            }
            // A daemon-schema field changed on the engine:
            // the readout the overlay renders from stays in sync, the routing fields
            // keep the display in sync, every other field's notice line came
            // from the shared mapping.
            EngineEvent::ConfigChanged { field, value, .. } => {
                self.settings.insert(field.clone(), value.clone());
                // a provider change may hide or reveal its role's model row (T3.2)
                if self.modals.contains(Modal::Overlay) {
                    self.overlay.sync_readout(&self.settings);
                }
                match field.as_str() {
                    "builder_provider" => {
                        self.run.config_provider = value.to_string();
                        self.run.provider = self.run.config_provider.clone();
                    }
                    "builder_model" => {
                        self.run.config_model = value.to_string();
                        self.run.model = self.run.config_model.clone();
                    }
                    _ => {}
                }
            }
            // The pipeline rail changed: the full state
            // replaces the previous one. Rendered by the pipeline rail of T50.1.
            EngineEvent::PipelineChanged { state } => self.pipeline = state,
            // One provider's discovery state replaces its previous state. The open
            // model picker reads `model_catalog` on every render and key, so this
            // insert alone updates it live; no overlay, draft or modal state is touched.
            EngineEvent::ModelCatalogChanged { catalog } => {
                self.model_catalog.insert(catalog.provider.clone(), catalog);
            }
            EngineEvent::Notice { .. } => {}
        }
        for line in out.lines {
            self.run.output.push(line);
        }
        if let Some(chunk) = out.append {
            self.run.output.append(LineOrigin::Agent, &chunk);
        }
    }

    pub(crate) fn push(&mut self, origin: LineOrigin, kind: LineKind, text: String) {
        self.run.output.push(OutLine { kind, origin, text });
    }

    /// The frame focus the engine state implies: the task list while idle, the agent
    /// output while a build, planner or discovery run is active.
    pub(crate) fn state_focus(&self) -> FrameFocus {
        if self.is_idle() {
            FrameFocus::Tasks
        } else {
            FrameFocus::Output
        }
    }

    /// Restores the state-implied focus when the idle/busy classification flips; a
    /// manual Tab or mouse choice persists while the state stays.
    fn sync_focus(&mut self, was_idle: bool) {
        if was_idle != self.is_idle() {
            self.focus = self.state_focus();
        }
    }

    /// Starts or stops the session timer when the idle/busy classification flips:
    /// the session runs from the idle→busy transition to the busy→idle one, so
    /// the timer hides while the engine is idle and restarts for the next session.
    /// The idle→busy flip fills a missing anchor only (a build start before the
    /// first announce, or a run whose `AgentChanged` carried `started_ms == 0`);
    /// an engine-recorded anchor always wins (T131.1, T42.1).
    fn sync_session(&mut self, was_idle: bool) {
        if was_idle != self.is_idle() {
            if was_idle {
                self.clock
                    .session_start
                    .get_or_insert_with(|| self.clock.now.get());
                self.clock
                    .overall_start
                    .get_or_insert_with(|| self.clock.now.get());
            } else {
                self.clock.session_start = None;
                self.clock.overall_start = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::{Duration, SystemTime};

    use patok_core::event::Snapshot;
    use patok_core::pipeline::PipelineState;

    fn app() -> App {
        App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: vec![],
                current_task: None,
                planning: false,
                discovering: false,
                provider: "claude".into(),
                model: String::new(),
                settings: BTreeMap::new(),
                pipeline: PipelineState::today(),
                model_catalog: Vec::new(),
                recent: vec![],
            },
            "0.1.0".into(),
        )
    }

    /// The current epoch in milliseconds, the unit `AgentChanged.started_ms`
    /// carries.
    fn epoch_ms() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    /// A `started_ms` this many milliseconds before now.
    fn started_ms_ago(ms: u64) -> u64 {
        epoch_ms().saturating_sub(ms)
    }

    /// The engine announces the run's agent before the run's flag flips
    /// (T24.1), so `AgentChanged` lands while the shell still classifies
    /// itself as idle. The engine-recorded start anchors the timer anyway,
    /// and the flag flip that follows must not overwrite it with the local
    /// clock (T131.1).
    #[test]
    fn the_timer_anchors_at_the_engine_recorded_agent_start() {
        let mut app = app();
        app.apply(EngineEvent::AgentChanged {
            agent: "planner".into(),
            started_ms: started_ms_ago(10_000),
        });
        app.apply(EngineEvent::PlanningChanged { planning: true });
        let elapsed = app.session_elapsed().expect("the timer runs");
        assert!(
            elapsed >= Duration::from_secs(9) && elapsed <= Duration::from_secs(11),
            "elapsed {elapsed:?}"
        );

        // The run's end still hides the timer (T42.1's idle contract).
        app.apply(EngineEvent::PlanningChanged { planning: false });
        assert_eq!(app.session_elapsed(), None);
    }

    /// A build's flag flip with no prior `AgentChanged` still anchors the
    /// timer at the local clock: the anchor is only filled when missing.
    #[test]
    fn the_flag_flip_fills_only_a_missing_anchor() {
        let mut app = app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        let elapsed = app.session_elapsed().expect("the timer runs");
        assert!(elapsed < Duration::from_secs(1), "elapsed {elapsed:?}");
    }

    /// A shell attaching mid-run replays the `AgentChanged` that names the
    /// running agent; with the guard gone the replayed engine start anchors
    /// the timer, so the timer counts from the run's true start, not from the
    /// attach (T131.1).
    #[test]
    fn an_attach_mid_run_counts_from_the_engine_start() {
        let app = App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: vec![],
                current_task: None,
                planning: true,
                discovering: false,
                provider: "claude".into(),
                model: String::new(),
                settings: BTreeMap::new(),
                pipeline: PipelineState::today(),
                model_catalog: Vec::new(),
                recent: vec![
                    EngineEvent::AgentChanged {
                        agent: "planner".into(),
                        started_ms: started_ms_ago(10_000),
                    },
                    EngineEvent::PlanningChanged { planning: true },
                ],
            },
            "0.1.0".into(),
        );
        let elapsed = app.session_elapsed().expect("the timer runs");
        assert!(
            elapsed >= Duration::from_secs(9) && elapsed <= Duration::from_secs(11),
            "elapsed {elapsed:?}"
        );
    }

    /// A snapshot that says the engine is idle clears any anchor a replayed
    /// `AgentChanged` from a finished session left behind, so the timer never
    /// shows a stale run's time and the next run starts fresh (T131.1).
    #[test]
    fn an_idle_attach_clears_a_stale_replayed_anchor() {
        let app = App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: vec![],
                current_task: None,
                planning: false,
                discovering: false,
                provider: "claude".into(),
                model: String::new(),
                settings: BTreeMap::new(),
                pipeline: PipelineState::today(),
                model_catalog: Vec::new(),
                recent: vec![EngineEvent::AgentChanged {
                    agent: "planner".into(),
                    started_ms: started_ms_ago(10_000),
                }],
            },
            "0.1.0".into(),
        );
        assert_eq!(app.session_elapsed(), None);
    }

    /// A `started_ms` of 0 means the engine could not record the start; the
    /// run's flag flip then fills the missing anchor at the local clock.
    #[test]
    fn an_unknown_start_relies_on_the_flag_flip() {
        let mut app = app();
        app.apply(EngineEvent::AgentChanged {
            agent: "planner".into(),
            started_ms: 0,
        });
        app.apply(EngineEvent::PlanningChanged { planning: true });
        let elapsed = app.session_elapsed().expect("the timer runs");
        assert!(elapsed < Duration::from_secs(1), "elapsed {elapsed:?}");
    }

    /// The overall timer keeps the run's first agent start across a stage
    /// change, while the per-agent timer re-anchors on the new agent.
    #[test]
    fn the_overall_timer_survives_an_agent_change() {
        let mut app = app();
        app.apply(EngineEvent::AgentChanged {
            agent: "planner".into(),
            started_ms: started_ms_ago(60_000),
        });
        app.apply(EngineEvent::PlanningChanged { planning: true });
        app.apply(EngineEvent::AgentChanged {
            agent: "builder".into(),
            started_ms: started_ms_ago(5_000),
        });
        let agent = app.session_elapsed().expect("the timer runs");
        let overall = app.overall_elapsed().expect("the timer runs");
        assert!(agent <= Duration::from_secs(6), "agent {agent:?}");
        assert!(overall >= Duration::from_secs(59), "overall {overall:?}");

        app.apply(EngineEvent::PlanningChanged { planning: false });
        assert_eq!(app.overall_elapsed(), None);
    }

    #[test]
    fn the_model_catalog_is_seeded_from_the_snapshot_and_replaced_per_provider() {
        use patok_core::event::{CatalogModel, ModelCatalogStatus, ProviderCatalog};

        let pending = ProviderCatalog {
            provider: "claude".into(),
            status: ModelCatalogStatus::Pending,
            ..ProviderCatalog::default()
        };
        let mut app = App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: vec![],
                current_task: None,
                planning: false,
                discovering: false,
                provider: "claude".into(),
                model: String::new(),
                settings: BTreeMap::new(),
                pipeline: PipelineState::today(),
                model_catalog: vec![pending.clone()],
                recent: vec![],
            },
            "0.1.0".into(),
        );
        assert_eq!(app.model_catalog.get("claude"), Some(&pending));

        let ready = ProviderCatalog {
            provider: "claude".into(),
            status: ModelCatalogStatus::Ready,
            error: String::new(),
            models: vec![CatalogModel {
                id: "sonnet".into(),
                display_name: Some("Sonnet".into()),
                listed: true,
            }],
        };
        app.apply(EngineEvent::ModelCatalogChanged {
            catalog: ready.clone(),
        });
        assert_eq!(app.model_catalog.get("claude"), Some(&ready));

        let failed = ProviderCatalog {
            provider: "codex".into(),
            status: ModelCatalogStatus::Failed,
            error: "no binary".into(),
            models: vec![],
        };
        app.apply(EngineEvent::ModelCatalogChanged {
            catalog: failed.clone(),
        });
        assert_eq!(app.model_catalog.len(), 2);
        assert_eq!(app.model_catalog.get("codex"), Some(&failed));
        assert_eq!(app.model_catalog.get("claude"), Some(&ready));
    }
}
