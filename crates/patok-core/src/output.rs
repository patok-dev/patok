//! The output pane's lines and the event-to-line mapping, shared by the
//! shell and (T184.2) the engine's history writer: [`Pane`] keeps the lines
//! a run produced together with its streaming-line state, and [`event_lines`]
//! maps one engine event onto the lines it adds. Each line records its
//! origin -- orchestrator or agent -- because [`LineKind::Error`] is produced
//! by both an orchestrator [`Notice`](crate::event::EngineEvent::Notice) and
//! an agent's `Stderr`/`Error` events.

use std::collections::VecDeque;
use std::time::Duration;

use crate::event::{AgentEvent, EngineEvent, NoticeLevel, SessionOutcome, TaskOutcome};
use crate::settings::ApplyTiming;

/// Output lines kept; older ones scroll off for good.
pub const OUTPUT_LIMIT: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Text,
    /// The agent's thinking; rendered as markdown in the thinking colour.
    Thinking,
    Tool,
    Result,
    Error,
    Notice,
    /// A status line, not agent content: the agent-started line (T78.1) and
    /// the finished-agent line naming the agent type and the session's
    /// duration (T42.1), in the heading colour (T96.1).
    Status,
    Heading,
}

/// Where an output line came from: the orchestrator (the engine's own
/// events and notices) or an agent's session output. [`LineKind::Error`] is
/// produced by both -- an orchestrator [`Notice`](crate::event::EngineEvent::Notice)
/// at [`NoticeLevel::Error`] and an agent's [`Stderr`](crate::event::AgentEvent::Stderr)
/// or [`Error`](crate::event::AgentEvent::Error) -- so the origin separates
/// them; `orchestrator.log` (T184.2) filters on it. Agent lines are Text,
/// Thinking, Tool, Result and agent Stderr/Error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineOrigin {
    Orchestrator,
    Agent,
}

/// One output line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutLine {
    pub kind: LineKind,
    /// Where the line came from; see [`LineOrigin`].
    pub origin: LineOrigin,
    pub text: String,
}

/// Agent output lines, with the streaming-line state deltas append to.
#[derive(Default)]
pub struct Pane {
    pub lines: VecDeque<OutLine>,
    /// Whether the last line is a streaming line that deltas append to.
    open: bool,
}

impl Pane {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Whether the last line is a streaming line that deltas append to; the
    /// open flag [`event_lines`] takes.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Pushes one completed line, closing the streaming line.
    pub fn push(&mut self, line: OutLine) {
        self.open = false;
        self.add(line);
    }

    fn add(&mut self, line: OutLine) {
        if self.lines.len() == OUTPUT_LIMIT {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Appends a streamed chunk to the current line, starting new lines at
    /// newlines; the lines it starts carry `origin`.
    pub fn append(&mut self, origin: LineOrigin, chunk: &str) {
        for (i, piece) in chunk.split('\n').enumerate() {
            if i > 0 {
                self.open = false;
            }
            if piece.is_empty() {
                continue;
            }
            match self.lines.back_mut() {
                Some(line) if self.open => line.text.push_str(piece),
                _ => self.add(OutLine {
                    kind: LineKind::Text,
                    origin,
                    text: piece.to_string(),
                }),
            }
            self.open = true;
        }
    }
}

/// The output frame title's agent type: the active agent's name capitalised
/// ("planner" -> "Planner", "builder" -> "Builder"; T11.1).
pub fn agent_display(agent: &str) -> String {
    let mut chars = agent.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A session's duration in human words (T42.1): `M sec` under a minute,
/// `N min M sec` under an hour (seconds zero-padded) and `N h M min` beyond
/// (minutes-within-the-hour zero-padded).
pub fn human_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (mins, secs) = (total / 60, total % 60);
    if total < 60 {
        format!("{secs} sec")
    } else if total < 3600 {
        format!("{mins} min {secs:02} sec")
    } else {
        format!("{} h {:02} min", total / 3600, mins % 60)
    }
}

/// The started line appended to the output pane when one agent session begins
/// (T78.1): the agent type name exactly as the finished line shows it, the
/// provider it runs on, and the configured model when one exists.
pub fn started_line(agent: &str, provider: &str, model: Option<&str>) -> String {
    match model {
        Some(model) => format!("{} started ({provider}, {model})", agent_display(agent)),
        None => format!("{} started ({provider})", agent_display(agent)),
    }
}

/// The finished line appended to the output pane when one agent session ends
/// (T42.1): the agent type name exactly as the frame title shows it, the
/// outcome's word (`finished`, `failed` or `cancelled`) and the human duration.
pub fn finished_line(agent: &str, outcome: SessionOutcome, duration: Duration) -> String {
    let word = match outcome {
        SessionOutcome::Done | SessionOutcome::Finished => "finished",
        SessionOutcome::Failed => "failed",
        SessionOutcome::Cancelled => "cancelled",
    };
    format!(
        "{} {word} in {}",
        agent_display(agent),
        human_duration(duration)
    )
}

/// What one engine event adds to the output pane.
#[derive(Debug, PartialEq, Eq)]
pub struct EventLines {
    /// The completed lines to push onto the pane, in order.
    pub lines: Vec<OutLine>,
    /// The streamed chunk an agent text delta merges into the pane's current
    /// line through [`Pane::append`]; `None` for every other event.
    pub append: Option<String>,
    /// The pane's open flag once this output is applied: a push closes the
    /// streaming line, an append keeps it open unless the chunk ends a
    /// line, and an event with no output leaves it alone.
    pub open: bool,
}

/// What one engine event adds to the output pane: the completed lines to
/// push, the streamed chunk to append and the pane's open flag once the
/// output is applied. Pure: the pane's contents are not needed, only its
/// open flag.
pub fn event_lines(event: &EngineEvent, open: bool) -> EventLines {
    match event {
        EngineEvent::TaskStarted { id, description } => line(
            LineOrigin::Orchestrator,
            LineKind::Heading,
            format!("── {id}: {description}"),
        ),
        EngineEvent::AgentStarted {
            agent,
            provider,
            model,
        } => line(
            LineOrigin::Orchestrator,
            LineKind::Status,
            started_line(agent, provider, model.as_deref()),
        ),
        EngineEvent::AgentFinished {
            agent,
            outcome,
            duration_ms,
        } => line(
            LineOrigin::Orchestrator,
            LineKind::Status,
            finished_line(agent, *outcome, Duration::from_millis(*duration_ms)),
        ),
        EngineEvent::TaskFinished {
            id,
            outcome,
            commit,
        } => {
            let sha = commit
                .as_ref()
                .map(|c| format!(" ({c})"))
                .unwrap_or_default();
            let (kind, text) = match outcome {
                TaskOutcome::Done | TaskOutcome::Finished => {
                    (LineKind::Notice, format!("✔ {id} done{sha}"))
                }
                TaskOutcome::Failed => (LineKind::Error, format!("✘ {id} failed{sha}")),
                TaskOutcome::Cancelled => (LineKind::Notice, format!("■ {id} cancelled")),
            };
            line(LineOrigin::Orchestrator, kind, text)
        }
        EngineEvent::ConfigChanged {
            field,
            value,
            timing,
        } => {
            // The routing fields update the run state, not the pane.
            if matches!(field.as_str(), "builder_provider" | "builder_model") {
                no_lines(open)
            } else {
                let mut text = format!("Setting `{field}` changed to `{value}`");
                if *timing == ApplyTiming::NextUnitOfWork {
                    text.push_str(" (takes effect on the next start)");
                }
                line(LineOrigin::Orchestrator, LineKind::Notice, text)
            }
        }
        EngineEvent::Notice { level, text } => {
            let kind = if *level == NoticeLevel::Error {
                LineKind::Error
            } else {
                LineKind::Notice
            };
            line(LineOrigin::Orchestrator, kind, text.clone())
        }
        EngineEvent::Agent { event } => agent_lines(event, open),
        // The run-state events add no lines: phase, flags, the active agent,
        // the task list and the pipeline rail live in the caller's state.
        EngineEvent::PhaseChanged { .. }
        | EngineEvent::AgentChanged { .. }
        | EngineEvent::TasksChanged { .. }
        | EngineEvent::PlanningChanged { .. }
        | EngineEvent::DiscoveryChanged { .. }
        | EngineEvent::PipelineChanged { .. }
        | EngineEvent::ModelCatalogChanged { .. } => no_lines(open),
    }
}

/// What one agent event adds to the output pane; agent lines carry
/// [`LineOrigin::Agent`].
fn agent_lines(event: &AgentEvent, open: bool) -> EventLines {
    match event {
        // A delta pushes no line: it merges into the pane's current line
        // through `Pane::append`, which owns the merge branch.
        AgentEvent::TextDelta { text } => EventLines {
            lines: vec![],
            append: Some(text.clone()),
            open: append_open(open, text),
        },
        AgentEvent::Text { text } => line(LineOrigin::Agent, LineKind::Text, text.clone()),
        AgentEvent::Thinking { text } => line(LineOrigin::Agent, LineKind::Thinking, text.clone()),
        AgentEvent::ToolUse { name, input } => line(
            LineOrigin::Agent,
            LineKind::Tool,
            format!("▸ {name} {input}"),
        ),
        AgentEvent::ToolResult { output } => {
            line(LineOrigin::Agent, LineKind::Result, format!("  ↳ {output}"))
        }
        AgentEvent::Stderr { text } | AgentEvent::Error { text, .. } => {
            line(LineOrigin::Agent, LineKind::Error, text.clone())
        }
        AgentEvent::Result { text } => {
            if text.is_empty() {
                no_lines(open)
            } else {
                line(
                    LineOrigin::Agent,
                    LineKind::Result,
                    format!("result: {text}"),
                )
            }
        }
        // Usage feeds the run state's readout, not the pane.
        AgentEvent::Usage(_) => no_lines(open),
    }
}

/// One completed line from `origin`; a push closes the streaming line.
fn line(origin: LineOrigin, kind: LineKind, text: String) -> EventLines {
    EventLines {
        lines: vec![OutLine { kind, origin, text }],
        append: None,
        open: false,
    }
}

/// No output: the pane's open flag passes through unchanged.
fn no_lines(open: bool) -> EventLines {
    EventLines {
        lines: vec![],
        append: None,
        open,
    }
}

/// The pane's open flag after appending `chunk` (mirrors [`Pane::append`]):
/// an empty chunk changes nothing, a chunk that ends a line closes the
/// streaming line, any other chunk leaves it open.
fn append_open(open: bool, chunk: &str) -> bool {
    if chunk.is_empty() {
        open
    } else {
        !chunk.ends_with('\n')
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{ErrorKind, Phase, Usage};
    use crate::pipeline::PipelineState;
    use crate::settings::SettingValue;

    /// The expected output of an event that pushes exactly one line.
    fn one(kind: LineKind, origin: LineOrigin, text: &str) -> EventLines {
        EventLines {
            lines: vec![OutLine {
                kind,
                origin,
                text: text.into(),
            }],
            append: None,
            open: false,
        }
    }

    #[test]
    fn human_duration_formats_seconds_minutes_and_hours() {
        assert_eq!(human_duration(Duration::from_secs(0)), "0 sec");
        assert_eq!(human_duration(Duration::from_secs(12)), "12 sec");
        assert_eq!(human_duration(Duration::from_secs(59)), "59 sec");
        assert_eq!(human_duration(Duration::from_secs(125)), "2 min 05 sec");
        assert_eq!(human_duration(Duration::from_secs(825)), "13 min 45 sec");
        assert_eq!(human_duration(Duration::from_secs(3600)), "1 h 00 min");
        assert_eq!(human_duration(Duration::from_secs(3900)), "1 h 05 min");
    }

    #[test]
    fn started_line_names_the_agent_provider_and_model() {
        assert_eq!(
            started_line("builder", "mock", Some("m-brand")),
            "Builder started (mock, m-brand)"
        );
        // No configured model: the model part is omitted cleanly (T78.1).
        assert_eq!(
            started_line("builder", "mock", None),
            "Builder started (mock)"
        );
        assert_eq!(
            started_line("planner", "vibe", Some("mistral-large")),
            "Planner started (vibe, mistral-large)"
        );
        assert_eq!(
            started_line("discovery", "claude", None),
            "Discovery started (claude)"
        );
    }

    #[test]
    fn finished_line_names_the_agent_and_the_outcome() {
        assert_eq!(
            finished_line(
                "builder",
                SessionOutcome::Finished,
                Duration::from_secs(125)
            ),
            "Builder finished in 2 min 05 sec"
        );
        assert_eq!(
            finished_line(
                "planner",
                SessionOutcome::Finished,
                Duration::from_secs(825)
            ),
            "Planner finished in 13 min 45 sec"
        );
        assert_eq!(
            finished_line("builder", SessionOutcome::Failed, Duration::from_secs(12)),
            "Builder failed in 12 sec"
        );
        assert_eq!(
            finished_line(
                "discovery",
                SessionOutcome::Cancelled,
                Duration::from_secs(45)
            ),
            "Discovery cancelled in 45 sec"
        );
    }

    #[test]
    fn push_closes_the_streaming_line() {
        let mut pane = Pane::default();
        pane.append(LineOrigin::Agent, "abc");
        assert!(pane.is_open());
        pane.push(OutLine {
            kind: LineKind::Notice,
            origin: LineOrigin::Orchestrator,
            text: "done".into(),
        });
        assert!(!pane.is_open());
        assert_eq!(pane.lines.len(), 2);
    }

    #[test]
    fn append_merges_into_the_open_line_and_starts_a_new_text_line_when_closed() {
        let mut pane = Pane::default();
        pane.append(LineOrigin::Agent, "Hel");
        pane.append(LineOrigin::Agent, "lo");
        assert_eq!(pane.lines.len(), 1);
        assert_eq!(pane.lines[0].kind, LineKind::Text);
        assert_eq!(pane.lines[0].origin, LineOrigin::Agent);
        assert_eq!(pane.lines[0].text, "Hello");

        // Once a push closed the streaming line, an append starts a new Text
        // line of the given origin instead of merging.
        pane.push(OutLine {
            kind: LineKind::Notice,
            origin: LineOrigin::Orchestrator,
            text: "done".into(),
        });
        pane.append(LineOrigin::Agent, "next");
        assert_eq!(pane.lines.len(), 3);
        assert_eq!(pane.lines[2].kind, LineKind::Text);
        assert_eq!(pane.lines[2].origin, LineOrigin::Agent);
        assert_eq!(pane.lines[2].text, "next");
    }

    #[test]
    fn append_splits_on_newlines_and_skips_empty_pieces() {
        let mut pane = Pane::default();
        pane.append(LineOrigin::Agent, "a\n\nb\n");
        let texts: Vec<&str> = pane.lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["a", "b"]);
        // The chunk ended a line, so the streaming line is closed.
        assert!(!pane.is_open());
    }

    #[test]
    fn an_empty_append_leaves_the_pane_alone() {
        let mut pane = Pane::default();
        pane.append(LineOrigin::Agent, "abc");
        assert!(pane.is_open());
        pane.append(LineOrigin::Agent, "");
        assert!(pane.is_open());
        assert_eq!(pane.lines.len(), 1);

        let mut pane = Pane::default();
        pane.append(LineOrigin::Agent, "");
        assert!(!pane.is_open());
        assert!(pane.is_empty());
    }

    #[test]
    fn the_pane_keeps_at_most_output_limit_lines() {
        let mut pane = Pane::default();
        for i in 0..=OUTPUT_LIMIT {
            pane.push(OutLine {
                kind: LineKind::Text,
                origin: LineOrigin::Agent,
                text: i.to_string(),
            });
        }
        assert_eq!(pane.lines.len(), OUTPUT_LIMIT);
        // The oldest line scrolled off for good.
        assert_eq!(pane.lines[0].text, "1");
        assert_eq!(pane.lines[OUTPUT_LIMIT - 1].text, OUTPUT_LIMIT.to_string());
    }

    #[test]
    fn task_started_makes_an_orchestrator_heading_that_closes_the_streaming_line() {
        let event = EngineEvent::TaskStarted {
            id: "T1.2".into(),
            description: "add the parser".into(),
        };
        assert_eq!(
            event_lines(&event, true),
            one(
                LineKind::Heading,
                LineOrigin::Orchestrator,
                "── T1.2: add the parser"
            )
        );
    }

    #[test]
    fn agent_started_and_finished_make_orchestrator_status_lines() {
        let started = EngineEvent::AgentStarted {
            agent: "builder".into(),
            provider: "mock".into(),
            model: Some("m-brand".into()),
        };
        assert_eq!(
            event_lines(&started, false),
            one(
                LineKind::Status,
                LineOrigin::Orchestrator,
                "Builder started (mock, m-brand)"
            )
        );
        let finished = EngineEvent::AgentFinished {
            agent: "builder".into(),
            outcome: SessionOutcome::Finished,
            duration_ms: 125_000,
        };
        assert_eq!(
            event_lines(&finished, false),
            one(
                LineKind::Status,
                LineOrigin::Orchestrator,
                "Builder finished in 2 min 05 sec"
            )
        );
    }

    #[test]
    fn task_finished_makes_orchestrator_notice_and_error_lines() {
        let done = EngineEvent::TaskFinished {
            id: "T1.2".into(),
            outcome: TaskOutcome::Done,
            commit: Some("abc1234".into()),
        };
        assert_eq!(
            event_lines(&done, false),
            one(
                LineKind::Notice,
                LineOrigin::Orchestrator,
                "✔ T1.2 done (abc1234)"
            )
        );
        let failed = EngineEvent::TaskFinished {
            id: "T1.2".into(),
            outcome: TaskOutcome::Failed,
            commit: None,
        };
        assert_eq!(
            event_lines(&failed, false),
            one(LineKind::Error, LineOrigin::Orchestrator, "✘ T1.2 failed")
        );
        let cancelled = EngineEvent::TaskFinished {
            id: "T1.2".into(),
            outcome: TaskOutcome::Cancelled,
            commit: None,
        };
        assert_eq!(
            event_lines(&cancelled, false),
            one(
                LineKind::Notice,
                LineOrigin::Orchestrator,
                "■ T1.2 cancelled"
            )
        );
    }

    #[test]
    fn config_changed_routes_builder_provider_and_model_away_from_the_pane() {
        for field in ["builder_provider", "builder_model"] {
            let event = EngineEvent::ConfigChanged {
                field: field.into(),
                value: SettingValue::Str("mock".into()),
                timing: ApplyTiming::Immediate,
            };
            assert_eq!(event_lines(&event, true), no_lines(true), "{event:?}");
        }
    }

    #[test]
    fn config_changed_makes_an_orchestrator_notice_with_its_apply_timing() {
        let immediate = EngineEvent::ConfigChanged {
            field: "agent_timeout_secs".into(),
            value: SettingValue::Uint(120),
            timing: ApplyTiming::Immediate,
        };
        assert_eq!(
            event_lines(&immediate, false),
            one(
                LineKind::Notice,
                LineOrigin::Orchestrator,
                "Setting `agent_timeout_secs` changed to `120`"
            )
        );
        let next = EngineEvent::ConfigChanged {
            field: "agent_timeout_secs".into(),
            value: SettingValue::Uint(120),
            timing: ApplyTiming::NextUnitOfWork,
        };
        assert_eq!(
            event_lines(&next, false),
            one(
                LineKind::Notice,
                LineOrigin::Orchestrator,
                "Setting `agent_timeout_secs` changed to `120` (takes effect on the next start)"
            )
        );
    }

    /// `LineKind::Error` is produced by both an orchestrator notice and an
    /// agent's stderr/error events; the origin separates them (T183.1).
    #[test]
    fn error_lines_record_whether_they_came_from_the_orchestrator_or_an_agent() {
        let notice = EngineEvent::Notice {
            level: NoticeLevel::Error,
            text: "engine blew up".into(),
        };
        assert_eq!(
            event_lines(&notice, false),
            one(LineKind::Error, LineOrigin::Orchestrator, "engine blew up")
        );
        let info = EngineEvent::Notice {
            level: NoticeLevel::Info,
            text: "hello".into(),
        };
        assert_eq!(
            event_lines(&info, false),
            one(LineKind::Notice, LineOrigin::Orchestrator, "hello")
        );
        let stderr = EngineEvent::Agent {
            event: AgentEvent::Stderr {
                text: "agent blew up".into(),
            },
        };
        assert_eq!(
            event_lines(&stderr, false),
            one(LineKind::Error, LineOrigin::Agent, "agent blew up")
        );
        let error = EngineEvent::Agent {
            event: AgentEvent::Error {
                kind: ErrorKind::ModelNotLoaded { model: None },
                text: "no model".into(),
            },
        };
        assert_eq!(
            event_lines(&error, false),
            one(LineKind::Error, LineOrigin::Agent, "no model")
        );
    }

    #[test]
    fn the_run_state_events_add_no_lines_and_leave_the_open_flag_alone() {
        let events = [
            EngineEvent::PhaseChanged {
                phase: Phase::Running,
            },
            EngineEvent::AgentChanged {
                agent: "builder".into(),
                started_ms: 0,
            },
            EngineEvent::TasksChanged { tasks: vec![] },
            EngineEvent::PlanningChanged { planning: true },
            EngineEvent::DiscoveryChanged { discovering: true },
            EngineEvent::PipelineChanged {
                state: PipelineState::today(),
            },
            EngineEvent::ModelCatalogChanged {
                catalog: crate::event::ProviderCatalog::default(),
            },
        ];
        for event in &events {
            assert_eq!(event_lines(event, true), no_lines(true), "{event:?}");
            assert_eq!(event_lines(event, false), no_lines(false), "{event:?}");
        }
    }

    #[test]
    fn agent_events_make_agent_lines() {
        let cases: [(AgentEvent, LineKind, &str); 5] = [
            (AgentEvent::Text { text: "hi".into() }, LineKind::Text, "hi"),
            (
                AgentEvent::Thinking { text: "hmm".into() },
                LineKind::Thinking,
                "hmm",
            ),
            (
                AgentEvent::ToolUse {
                    name: "Bash".into(),
                    input: "ls".into(),
                },
                LineKind::Tool,
                "▸ Bash ls",
            ),
            (
                AgentEvent::ToolResult {
                    output: "done".into(),
                },
                LineKind::Result,
                "  ↳ done",
            ),
            (
                AgentEvent::Result {
                    text: "the answer".into(),
                },
                LineKind::Result,
                "result: the answer",
            ),
        ];
        for (event, kind, text) in cases {
            let event = EngineEvent::Agent { event };
            assert_eq!(
                event_lines(&event, false),
                one(kind, LineOrigin::Agent, text),
                "{event:?}"
            );
        }
    }

    #[test]
    fn an_empty_agent_result_and_usage_add_no_lines() {
        let empty = EngineEvent::Agent {
            event: AgentEvent::Result {
                text: String::new(),
            },
        };
        assert_eq!(event_lines(&empty, true), no_lines(true));
        let usage = EngineEvent::Agent {
            event: AgentEvent::Usage(Usage::default()),
        };
        assert_eq!(event_lines(&usage, false), no_lines(false));
    }

    #[test]
    fn a_text_delta_appends_and_tracks_the_open_flag() {
        let delta = |text: &str| EngineEvent::Agent {
            event: AgentEvent::TextDelta { text: text.into() },
        };
        // A chunk that does not end a line leaves the streaming line open.
        let out = event_lines(&delta("abc"), true);
        assert_eq!(out.append.as_deref(), Some("abc"));
        assert!(out.lines.is_empty());
        assert!(out.open);
        // A chunk that ends a line closes it.
        assert!(!event_lines(&delta("abc\n"), true).open);
        // An empty chunk changes nothing, open flag included.
        let out = event_lines(&delta(""), true);
        assert_eq!(out.append.as_deref(), Some(""));
        assert!(out.open);
        assert!(!event_lines(&delta(""), false).open);
        // Appending to a closed pane opens the streaming line again.
        let out = event_lines(&delta("abc"), false);
        assert_eq!(out.append.as_deref(), Some("abc"));
        assert!(out.open);
    }

    /// The safety net for the TUI rewiring (T183.2): feeding a run's events
    /// through `event_lines` into a real `Pane` produces exactly the lines
    /// the shell's reducer shows.
    #[test]
    fn a_run_through_event_lines_into_a_pane_shows_the_same_lines() {
        let mut pane = Pane::default();
        let events = [
            EngineEvent::TaskStarted {
                id: "T1.2".into(),
                description: "add the parser".into(),
            },
            EngineEvent::AgentStarted {
                agent: "builder".into(),
                provider: "mock".into(),
                model: None,
            },
            EngineEvent::Agent {
                event: AgentEvent::TextDelta { text: "Hel".into() },
            },
            EngineEvent::Agent {
                event: AgentEvent::TextDelta { text: "lo".into() },
            },
            EngineEvent::Agent {
                event: AgentEvent::Text {
                    text: "working on it".into(),
                },
            },
            EngineEvent::AgentFinished {
                agent: "builder".into(),
                outcome: SessionOutcome::Finished,
                duration_ms: 125_000,
            },
            EngineEvent::TaskFinished {
                id: "T1.2".into(),
                outcome: TaskOutcome::Done,
                commit: None,
            },
        ];
        for event in &events {
            let out = event_lines(event, pane.is_open());
            for line in out.lines {
                pane.push(line);
            }
            if let Some(chunk) = out.append {
                pane.append(LineOrigin::Agent, &chunk);
            }
            assert_eq!(pane.is_open(), out.open, "{event:?}");
        }
        let actual: Vec<(LineKind, LineOrigin, String)> = pane
            .lines
            .iter()
            .map(|line| (line.kind, line.origin, line.text.clone()))
            .collect();
        assert_eq!(
            actual,
            [
                (
                    LineKind::Heading,
                    LineOrigin::Orchestrator,
                    "── T1.2: add the parser".to_string()
                ),
                (
                    LineKind::Status,
                    LineOrigin::Orchestrator,
                    "Builder started (mock)".to_string()
                ),
                (LineKind::Text, LineOrigin::Agent, "Hello".to_string()),
                (
                    LineKind::Text,
                    LineOrigin::Agent,
                    "working on it".to_string()
                ),
                (
                    LineKind::Status,
                    LineOrigin::Orchestrator,
                    "Builder finished in 2 min 05 sec".to_string()
                ),
                (
                    LineKind::Notice,
                    LineOrigin::Orchestrator,
                    "✔ T1.2 done".to_string()
                ),
            ]
        );
    }
}
