//! The normalised agent event vocabulary and the events the engine pushes to the
//! shell. The wire format carries these as JSON in the opaque `bytes` fields of the proto.

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use crate::config::{ApplyTiming, SettingValue};
use crate::pipeline::PipelineState;
use crate::task::Task;

/// Token accounting of one agent session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub context_window: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

/// Typed error kinds recognised in provider error text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ErrorKind {
    ContextOverflow {
        requested: Option<u64>,
        available: Option<u64>,
    },
    ProviderUnreachable {
        url: Option<String>,
    },
    ModelNotLoaded {
        model: Option<String>,
    },
}

/// One item of the common event vocabulary every provider translates its native stream into.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// A complete assistant message or status line.
    Text {
        text: String,
    },
    /// An incremental chunk to append to the current line.
    TextDelta {
        text: String,
    },
    /// The agent's thinking/reasoning for one block, as markdown; rendered distinctly.
    Thinking {
        text: String,
    },
    /// Tool name plus a short input preview (up to 120 characters).
    ToolUse {
        name: String,
        input: String,
    },
    /// Output preview (up to 200 characters).
    ToolResult {
        output: String,
    },
    /// A raw error or warning line.
    Stderr {
        text: String,
    },
    /// The final answer text.
    Result {
        text: String,
    },
    Usage(Usage),
    Error {
        kind: ErrorKind,
        text: String,
    },
}

/// Truncates to at most `max` characters, marking the cut with an ellipsis.
pub fn preview(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Classifies error text into at most one typed kind, in priority order: context overflow,
/// provider unreachable, model not loaded.
pub fn classify_error(text: &str) -> Option<ErrorKind> {
    let lower = text.to_lowercase();
    if let Some((requested, available)) = parse_context_overflow(&lower) {
        return Some(ErrorKind::ContextOverflow {
            requested,
            available,
        });
    }
    if lower.contains("exceeds the available context size") || lower.contains("n_ctx_slot") {
        return Some(ErrorKind::ContextOverflow {
            requested: None,
            available: None,
        });
    }
    if ["connection refused", "econnrefused", "failed to connect"]
        .iter()
        .any(|m| lower.contains(m))
    {
        return Some(ErrorKind::ProviderUnreachable {
            url: first_url(text),
        });
    }
    if ["model not loaded", "no model loaded", "model_not_found"]
        .iter()
        .any(|m| lower.contains(m))
        || (lower.contains("404") && lower.contains("/v1/models"))
    {
        return Some(ErrorKind::ModelNotLoaded { model: None });
    }
    None
}

/// Parses "request N tokens exceeds the available context size (M tokens)".
fn parse_context_overflow(lower: &str) -> Option<(Option<u64>, Option<u64>)> {
    let before = lower
        .split("tokens exceeds the available context size")
        .next()?;
    if before.len() == lower.len() {
        return None;
    }
    let requested = before.trim_end().rsplit(' ').next()?.parse().ok()?;
    let after = lower.split("context size (").nth(1)?;
    let available = after.split(' ').next()?.parse().ok()?;
    Some((Some(requested), Some(available)))
}

fn first_url(text: &str) -> Option<String> {
    let start = text.find("http://").or_else(|| text.find("https://"))?;
    let url = text[start..].split_whitespace().next()?;
    Some(
        url.trim_end_matches(['.', ',', ';', ':', ')', '"', '\''])
            .to_string(),
    )
}

/// Engine phases. Planning arrives with a later MVP.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Startup,
    Running,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    /// Agent completed; task ticked and committed as `feat`.
    Done,
    /// Agent failed; progress committed as `WIP`, task stays pending.
    Failed,
    /// The agent was cancelled; nothing was committed.
    Cancelled,
}

/// How one agent session ended: the wording of the finished line the shell
/// appends to the output pane (T42.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionOutcome {
    /// The session ran to completion.
    Finished,
    /// The session errored or failed.
    Failed,
    /// The session was cancelled.
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// Everything the engine pushes to an attached shell after the initial snapshot.
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
    fn preview_truncates_and_flattens() {
        assert_eq!(preview("a\n  b", 10), "a b");
        assert_eq!(preview("abcdef", 4), "abc…");
    }

    #[test]
    fn classifies_context_overflow_with_counts() {
        let kind =
            classify_error("Request 9000 tokens exceeds the available context size (4096 tokens)");
        assert_eq!(
            kind,
            Some(ErrorKind::ContextOverflow {
                requested: Some(9000),
                available: Some(4096)
            })
        );
        assert_eq!(
            classify_error("slot n_ctx_slot too small"),
            Some(ErrorKind::ContextOverflow {
                requested: None,
                available: None
            })
        );
    }

    #[test]
    fn classifies_unreachable_with_url_hint() {
        let kind = classify_error("failed to connect to http://127.0.0.1:1234/v1.");
        assert_eq!(
            kind,
            Some(ErrorKind::ProviderUnreachable {
                url: Some("http://127.0.0.1:1234/v1".into())
            })
        );
    }

    #[test]
    fn classifies_model_not_loaded_and_untyped() {
        assert!(matches!(
            classify_error("Model not loaded"),
            Some(ErrorKind::ModelNotLoaded { .. })
        ));
        assert!(matches!(
            classify_error("404 on /v1/models"),
            Some(ErrorKind::ModelNotLoaded { .. })
        ));
        assert_eq!(classify_error("something else"), None);
    }

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
            recent: vec![],
        };
        let decoded = Snapshot::from_state(&snapshot.to_state()).unwrap();
        assert_eq!(decoded, snapshot);
        assert_eq!(
            decoded.settings.get("run_mode"),
            Some(&SettingValue::Str("sprint".into()))
        );
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
    }
}
