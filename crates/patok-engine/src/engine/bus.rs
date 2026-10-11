use std::time::Instant;

use patok_core::event::{AgentEvent, AgentRole, EngineEvent, NoticeLevel, SessionOutcome};

use super::Engine;
use super::state::State;
use crate::artifact;

/// Events kept for replay to a reattaching shell.
const RECENT_LIMIT: usize = 2000;

impl Engine {
    /// Records the event for replay and pushes it to the attached shell, atomically with
    /// respect to [`Engine::attach`] so a snapshot never misses or duplicates an event.
    pub(crate) fn emit_locked(&self, state: &mut State, event: EngineEvent) {
        match (&event, state.recent.back_mut()) {
            (
                EngineEvent::Agent {
                    event: AgentEvent::TextDelta { text },
                },
                Some(EngineEvent::Agent {
                    event: AgentEvent::TextDelta { text: last },
                }),
            ) => last.push_str(text),
            _ => {
                if state.recent.len() == RECENT_LIMIT {
                    state.recent.pop_front();
                }
                state.recent.push_back(event.clone());
            }
        }
        // No receiver is normal: nobody is attached.
        let _ = self.inner.events.send(event);
    }

    pub(crate) fn emit(&self, event: EngineEvent) {
        let mut state = self.state();
        self.emit_locked(&mut state, event);
    }

    pub(crate) fn notice(&self, level: NoticeLevel, text: impl Into<String>) {
        self.emit(EngineEvent::Notice {
            level,
            text: text.into(),
        });
    }

    /// Announces `role`'s agent as the engine's active agent, carrying the session's
    /// engine-recorded start (T42.1), and returns the instant the session's
    /// duration is measured from.
    pub(crate) fn announce_session(
        &self,
        role: AgentRole,
        provider: &str,
        model: Option<&str>,
    ) -> Instant {
        let mut state = self.state();
        self.announce_session_locked(&mut state, role, provider, model)
    }

    /// [`Engine::announce_session`] under an already-held state lock: announces
    /// `role`'s agent as the active agent before a run's state flag flips, so the
    /// shell can label the run from the moment it starts. The starting line
    /// (T78.1) names the provider and model the run itself uses, so a settings
    /// change mid-run cannot mislabel a later run. The wire boundary: the
    /// event's `agent` field carries the role's spelling.
    pub(crate) fn announce_session_locked(
        &self,
        state: &mut State,
        role: AgentRole,
        provider: &str,
        model: Option<&str>,
    ) -> Instant {
        self.emit_locked(
            state,
            EngineEvent::AgentStarted {
                agent: role.as_str().to_string(),
                provider: provider.to_string(),
                model: model.map(str::to_string),
            },
        );
        let started_ms = artifact::unix_now().as_millis() as u64;
        self.emit_locked(
            state,
            EngineEvent::AgentChanged {
                agent: role.as_str().to_string(),
                started_ms,
            },
        );
        Instant::now()
    }

    /// Pushes one agent session's end to the attached shells and the replayed
    /// recent events: the agent type name, how it ended and the total duration
    /// measured from `start` (T42.1).
    pub(crate) fn finish_session(&self, role: AgentRole, start: Instant, outcome: SessionOutcome) {
        self.emit(EngineEvent::AgentFinished {
            agent: role.as_str().to_string(),
            outcome,
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }
}
