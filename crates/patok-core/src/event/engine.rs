//! The events the engine pushes to the shell after the initial snapshot.

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use crate::pipeline::PipelineState;
use crate::settings::{ApplyTiming, SettingValue};
use crate::task::Task;

use super::agent::AgentEvent;

/// Engine phases. Planning arrives with a later MVP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Startup,
    Running,
}

/// How a task or an agent session ended. The two vocabularies share
/// `failed` and `cancelled` and differ only in the success spelling:
/// [`EngineEvent::TaskFinished`] carries `Done`, [`EngineEvent::AgentFinished`]
/// carries `Finished` (the wire spellings are pinned by the tests below).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// A task completed; ticked and committed as `feat`.
    Done,
    /// A session ran to completion.
    Finished,
    /// Agent failed; progress committed as `WIP`, task stays pending.
    Failed,
    /// The agent was cancelled; nothing was committed.
    Cancelled,
}

pub type TaskOutcome = Outcome;
pub type SessionOutcome = Outcome;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// Everything the engine pushes to attached shells after the initial snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EngineEvent {
    PhaseChanged {
        phase: Phase,
    },
    TaskStarted {
        id: String,
        description: String,
    },
    Agent {
        event: AgentEvent,
    },
    /// The engine's active agent changed: the plan stage's planner session, or the
    /// builder session running after it (T11.1). Replayed with the recent events so
    /// a reattaching shell shows the right agent. `started_ms` is the session's
    /// engine-recorded start as epoch milliseconds (T42.1); `0` means unknown, and
    /// a shell anchors its running timer on the value when it is set.
    AgentChanged {
        agent: String,
        #[serde(default)]
        started_ms: u64,
    },
    /// One agent session began (T78.1): the agent type name, the provider it runs
    /// on and the configured model when one exists. Emitted before the session is
    /// created, so a run that later fails or is interrupted still leaves its
    /// starting line. Replayed with the recent events, so the pair with
    /// [`EngineEvent::AgentFinished`] survives a reattach.
    AgentStarted {
        agent: String,
        provider: String,
        model: Option<String>,
    },
    /// One agent session ended (T42.1): the agent type name, how it ended and the
    /// session's total duration. Replayed with the recent events, so a shell that
    /// attached late still shows the finished line.
    AgentFinished {
        agent: String,
        outcome: SessionOutcome,
        duration_ms: u64,
    },
    /// The task list as read from disk after the engine changed it.
    TasksChanged {
        tasks: Vec<Task>,
    },
    /// The planner started or finished.
    PlanningChanged {
        planning: bool,
    },
    /// A discovery round started or finished.
    DiscoveryChanged {
        discovering: bool,
    },
    TaskFinished {
        id: String,
        outcome: TaskOutcome,
        commit: Option<String>,
    },
    /// A daemon-schema field's effective value changed: a
    /// settings change persisted by the engine, or a hand-edited config file picked up by
    /// the reload. Carries the field's post-merge, post-normalization value and when it
    /// takes effect, so the shell stays in sync without re-deriving either.
    ConfigChanged {
        field: String,
        value: SettingValue,
        timing: ApplyTiming,
    },
    /// The pipeline rail changed: the full state
    /// replaces the previous one. Replayed with the recent events so a
    /// reattaching shell shows the right rail.
    PipelineChanged {
        state: PipelineState,
    },
    /// One provider's model discovery state changed; it replaces that provider's
    /// previous state. The provider slug is `catalog.provider`.
    ModelCatalogChanged {
        catalog: ProviderCatalog,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
}

impl EngineEvent {
    /// The event name carried in the proto's `Event.kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::PhaseChanged { .. } => "phase_changed",
            Self::TaskStarted { .. } => "task_started",
            Self::Agent { .. } => "agent",
            Self::AgentChanged { .. } => "agent_changed",
            Self::AgentStarted { .. } => "agent_started",
            Self::AgentFinished { .. } => "agent_finished",
            Self::TasksChanged { .. } => "tasks_changed",
            Self::PlanningChanged { .. } => "planning_changed",
            Self::DiscoveryChanged { .. } => "discovery_changed",
            Self::TaskFinished { .. } => "task_finished",
            Self::ConfigChanged { .. } => "config_changed",
            Self::PipelineChanged { .. } => "pipeline_changed",
            Self::ModelCatalogChanged { .. } => "model_catalog_changed",
            Self::Notice { .. } => "notice",
        }
    }

    pub fn to_payload(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("engine events always serialise")
    }

    pub fn from_payload(payload: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(payload)
    }
}

/// A provider's model discovery status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCatalogStatus {
    #[default]
    NotDiscovered,
    Pending,
    Ready,
    Failed,
}

/// One discovered model. Mirrors `patok_providers::DiscoveredModel`, which core
/// cannot name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub listed: bool,
}

/// One provider's model discovery state. `provider` is the canonical slug
/// (`claude`, `codex`, `vibe`), because core cannot name `ProviderName`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCatalog {
    pub provider: String,
    #[serde(default)]
    pub status: ModelCatalogStatus,
    /// Why discovery failed; empty unless `Failed`.
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub models: Vec<CatalogModel>,
}

/// Full engine state at attach time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub project_dir: String,
    pub phase: Phase,
    pub tasks: Vec<Task>,
    pub current_task: Option<String>,
    /// A planning run is active.
    #[serde(default)]
    pub planning: bool,
    /// A discovery round is active.
    #[serde(default)]
    pub discovering: bool,
    /// Name of the active provider (empty when the engine has none).
    #[serde(default)]
    pub provider: String,
    /// The configured model name (empty when the provider picks its own default).
    #[serde(default)]
    pub model: String,
    /// The engine-reported daemon readout: every registry field with its effective value
    /// (T15.1's settings overlay renders its rows from this map, and updates them from
    /// `ConfigChanged` events).
    #[serde(default)]
    pub settings: BTreeMap<String, SettingValue>,
    /// The pipeline rail: the enabled stage tiles in order
    /// plus the three standalone tiles, as the engine computed it at attach time.
    #[serde(default)]
    pub pipeline: PipelineState,
    /// Every provider's discovery state at attach time; later changes arrive as
    /// `ModelCatalogChanged`.
    #[serde(default)]
    pub model_catalog: Vec<ProviderCatalog>,
    /// Recent events (bounded), replayed so a reattaching shell sees the running agent's output.
    pub recent: Vec<EngineEvent>,
}

impl Snapshot {
    pub fn to_state(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("snapshots always serialise")
    }

    pub fn from_state(state: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{Stage, Tile, TileStatus};

    #[test]
    fn engine_event_payload_round_trips() {
        let event = EngineEvent::Agent {
            event: AgentEvent::ToolUse {
                name: "Bash".into(),
                input: "ls".into(),
            },
        };
        assert_eq!(event.kind(), "agent");
        assert_eq!(
            EngineEvent::from_payload(&event.to_payload()).unwrap(),
            event
        );
    }

    #[test]
    fn config_changed_round_trips_with_kind() {
        let event = EngineEvent::ConfigChanged {
            field: "agent_timeout_secs".into(),
            value: SettingValue::Uint(120),
            timing: ApplyTiming::NextUnitOfWork,
        };
        assert_eq!(event.kind(), "config_changed");
        assert_eq!(
            EngineEvent::from_payload(&event.to_payload()).unwrap(),
            event
        );
    }

    #[test]
    fn agent_finished_round_trips_with_kind() {
        let event = EngineEvent::AgentFinished {
            agent: "builder".into(),
            outcome: SessionOutcome::Failed,
            duration_ms: 125_000,
        };
        assert_eq!(event.kind(), "agent_finished");
        assert_eq!(
            EngineEvent::from_payload(&event.to_payload()).unwrap(),
            event
        );
    }

    #[test]
    fn the_outcome_wire_spellings_stay_pinned() {
        let task = EngineEvent::TaskFinished {
            id: "T1.1".into(),
            outcome: TaskOutcome::Done,
            commit: None,
        };
        let json = String::from_utf8(task.to_payload()).unwrap();
        assert!(json.contains(r#""outcome":"done""#), "{json}");
        let agent = EngineEvent::AgentFinished {
            agent: "builder".into(),
            outcome: SessionOutcome::Finished,
            duration_ms: 1,
        };
        let json = String::from_utf8(agent.to_payload()).unwrap();
        assert!(json.contains(r#""outcome":"finished""#), "{json}");
    }

    #[test]
    fn an_agent_changed_without_started_ms_decodes_as_unknown() {
        let legacy = r#"{"kind":"agent_changed","agent":"planner"}"#;
        let event = EngineEvent::from_payload(legacy.as_bytes()).unwrap();
        assert_eq!(
            event,
            EngineEvent::AgentChanged {
                agent: "planner".into(),
                started_ms: 0,
            }
        );
    }

    #[test]
    fn an_agent_changed_carries_the_session_start() {
        let event = EngineEvent::AgentChanged {
            agent: "builder".into(),
            started_ms: 1_700_000_000_000,
        };
        assert_eq!(
            EngineEvent::from_payload(&event.to_payload()).unwrap(),
            event
        );
    }

    #[test]
    fn snapshot_round_trips_with_the_settings_readout() {
        let snapshot = Snapshot {
            project_dir: "/p".into(),
            phase: Phase::Startup,
            tasks: vec![],
            current_task: None,
            planning: false,
            discovering: false,
            provider: "claude".into(),
            model: String::new(),
            settings: [("run_mode".to_string(), SettingValue::Str("sprint".into()))]
                .into_iter()
                .collect(),
            pipeline: PipelineState::today(),
            model_catalog: vec![ProviderCatalog {
                provider: "codex".into(),
                status: ModelCatalogStatus::Ready,
                error: String::new(),
                models: vec![CatalogModel {
                    id: "gpt".into(),
                    display_name: None,
                    listed: true,
                }],
            }],
            recent: vec![],
        };
        let decoded = Snapshot::from_state(&snapshot.to_state()).unwrap();
        assert_eq!(decoded, snapshot);
        assert_eq!(
            decoded.settings.get("run_mode"),
            Some(&SettingValue::Str("sprint".into()))
        );
        assert_eq!(decoded.model_catalog.len(), 1);
    }

    #[test]
    fn pipeline_changed_round_trips_with_kind() {
        let event = EngineEvent::PipelineChanged {
            state: PipelineState {
                stages: vec![Tile {
                    stage: Stage::Build,
                    status: TileStatus::Active,
                }],
                ..PipelineState::today()
            },
        };
        assert_eq!(event.kind(), "pipeline_changed");
        assert_eq!(
            EngineEvent::from_payload(&event.to_payload()).unwrap(),
            event
        );
    }

    #[test]
    fn an_old_snapshot_without_the_settings_field_still_decodes() {
        let legacy = concat!(
            r#"{"project_dir":"/p","phase":"startup","tasks":[],"current_task":null,"#,
            r#""recent":[]}"#
        );
        let snapshot = Snapshot::from_state(legacy.as_bytes()).unwrap();
        assert_eq!(snapshot.project_dir, "/p");
        assert!(snapshot.settings.is_empty());
        assert!(snapshot.model_catalog.is_empty());
    }

    #[test]
    fn model_catalog_changed_round_trips_with_kind() {
        let failed = EngineEvent::ModelCatalogChanged {
            catalog: ProviderCatalog {
                provider: "claude".into(),
                status: ModelCatalogStatus::Failed,
                error: "binary missing".into(),
                models: vec![],
            },
        };
        let ready = EngineEvent::ModelCatalogChanged {
            catalog: ProviderCatalog {
                provider: "codex".into(),
                status: ModelCatalogStatus::Ready,
                error: String::new(),
                models: vec![
                    CatalogModel {
                        id: "a".into(),
                        display_name: Some("A".into()),
                        listed: true,
                    },
                    CatalogModel {
                        id: "b".into(),
                        display_name: None,
                        listed: false,
                    },
                ],
            },
        };
        for event in [failed, ready] {
            assert_eq!(event.kind(), "model_catalog_changed");
            assert_eq!(
                EngineEvent::from_payload(&event.to_payload()).unwrap(),
                event
            );
        }
    }

    #[test]
    fn the_model_catalog_status_wire_spellings_stay_pinned() {
        for (status, spelling) in [
            (ModelCatalogStatus::NotDiscovered, "not_discovered"),
            (ModelCatalogStatus::Pending, "pending"),
            (ModelCatalogStatus::Ready, "ready"),
            (ModelCatalogStatus::Failed, "failed"),
        ] {
            let event = EngineEvent::ModelCatalogChanged {
                catalog: ProviderCatalog {
                    provider: "claude".into(),
                    status,
                    ..ProviderCatalog::default()
                },
            };
            let json = String::from_utf8(event.to_payload()).unwrap();
            assert!(
                json.contains(&format!(r#""status":"{spelling}""#)),
                "{json}"
            );
        }
    }
}
