//! The event reducer (finding 19, the rest, in
//! docs/private/structure-review-followup.md): the one place engine events
//! fold into the shell's run state -- the phase and run flags, the active
//! agent, provider and model, the task list and the output pane -- plus the
//! reducer's own focus and session-timer helpers, split out of `app` so the
//! state types and the reducer each read as one thing. Pure: no I/O, so it
//! is tested without a terminal.

use std::time::Duration;

use patok_core::config::ApplyTiming;
use patok_core::event::{AgentEvent, EngineEvent, NoticeLevel, Phase, TaskOutcome};

use crate::app::{App, LineKind, finished_line, started_line};
use crate::view::FrameFocus;

impl App {
    pub fn apply(&mut self, event: EngineEvent) {
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
            EngineEvent::TaskStarted { id, description } => {
                self.run.current_task = Some(id.clone());
                self.push(LineKind::Heading, format!("── {id}: {description}"));
            }
            EngineEvent::Agent { event } => self.apply_agent(event),
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
                    self.clock.session_start = Some(self.clock.anchored(started_ms));
                }
            }
            // One agent session began: one line naming the agent type, the
            // provider it runs on and the configured model when one exists,
            // pairing with the finished line at the session's end (T78.1). The
            // active agent and the timer stay owned by the adjacent
            // `AgentChanged` arm. The session's provider and model drive the
            // output frame title while it runs, so a reviewer on another
            // provider (a `claude` reviewer provider) is titled accordingly instead
            // of silently reusing the configured one (T81.1).
            EngineEvent::AgentStarted {
                agent,
                provider,
                model,
            } => {
                let text = started_line(&agent, &provider, model.as_deref());
                self.push(LineKind::Status, text);
                self.run.provider = provider;
                self.run.model = model.unwrap_or_default();
            }
            // One agent session ended: one line naming the agent type and the
            // session's total duration lands at the end of the pane (T42.1). The
            // timer hides with the idle transition that follows, and the frame
            // title returns to the configured provider and model (T81.1).
            EngineEvent::AgentFinished {
                agent,
                outcome,
                duration_ms,
            } => {
                let text = finished_line(&agent, outcome, Duration::from_millis(duration_ms));
                self.push(LineKind::Status, text);
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
            EngineEvent::TaskFinished {
                id,
                outcome,
                commit,
            } => {
                self.run.current_task = None;
                let sha = commit.map(|c| format!(" ({c})")).unwrap_or_default();
                let (kind, text) = match outcome {
                    TaskOutcome::Done | TaskOutcome::Finished => {
                        self.run.completed += 1;
                        (LineKind::Notice, format!("✔ {id} done{sha}"))
                    }
                    TaskOutcome::Failed => (LineKind::Error, format!("✘ {id} failed{sha}")),
                    TaskOutcome::Cancelled => (LineKind::Notice, format!("■ {id} cancelled")),
                };
                self.push(kind, text);
            }
            // A daemon-schema field changed on the engine:
            // the readout the overlay renders from stays in sync, the routing fields
            // keep the display in sync, every other field is a notice line with its
            // apply timing.
            EngineEvent::ConfigChanged {
                field,
                value,
                timing,
            } => {
                self.settings.insert(field.clone(), value.clone());
                match field.as_str() {
                    "provider" => {
                        self.run.config_provider = value.to_string();
                        self.run.provider = self.run.config_provider.clone();
                    }
                    "model" => {
                        self.run.config_model = value.to_string();
                        self.run.model = self.run.config_model.clone();
                    }
                    _ => {
                        let mut text = format!("Setting `{field}` changed to `{value}`");
                        if timing == ApplyTiming::NextUnitOfWork {
                            text.push_str(" (takes effect on the next start)");
                        }
                        self.push(LineKind::Notice, text);
                    }
                }
            }
            // The pipeline rail changed: the full state
            // replaces the previous one. Rendered by the pipeline rail of T50.1.
            EngineEvent::PipelineChanged { state } => self.pipeline = state,
            EngineEvent::Notice { level, text } => {
                let kind = if level == NoticeLevel::Error {
                    LineKind::Error
                } else {
                    LineKind::Notice
                };
                self.push(kind, text);
            }
        }
    }

    fn apply_agent(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextDelta { text } => self.append(&text),
            AgentEvent::Text { text } => self.push(LineKind::Text, text),
            AgentEvent::Thinking { text } => self.push(LineKind::Thinking, text),
            AgentEvent::ToolUse { name, input } => {
                self.push(LineKind::Tool, format!("▸ {name} {input}"))
            }
            AgentEvent::ToolResult { output } => {
                self.push(LineKind::Result, format!("  ↳ {output}"))
            }
            AgentEvent::Stderr { text } | AgentEvent::Error { text, .. } => {
                self.push(LineKind::Error, text)
            }
            AgentEvent::Result { text } => {
                if !text.is_empty() {
                    self.push(LineKind::Result, format!("result: {text}"));
                }
            }
            AgentEvent::Usage(usage) if !self.run.planning => self.run.usage = Some(usage),
            AgentEvent::Usage(_) => {}
        }
    }

    pub(crate) fn push(&mut self, kind: LineKind, text: String) {
        self.run.output.push(kind, text);
    }

    fn append(&mut self, chunk: &str) {
        self.run.output.append(chunk);
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
            } else {
                self.clock.session_start = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::SystemTime;

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
}
