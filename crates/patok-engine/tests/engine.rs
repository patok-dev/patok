//! Engine behaviour against the mock provider and a real temporary git repository.

mod support;

use std::path::PathBuf;
use std::time::Duration;

use patok_core::event::{AgentEvent, EngineEvent, NoticeLevel, Phase, SessionOutcome, TaskOutcome};
use patok_core::pipeline::{PipelineState, Stage, Tile, TileStatus};
use patok_engine::{QueueCreation, TASK_FILE};
use patok_providers::ExitKind;
use patok_providers::RESEARCH_TOOLS;
use patok_providers::mock::{MockProvider, Step};
use support::{Fixture, collect_until, is_phase_startup};

const TASKS: &str =
    "# Tasks\n\n## Phase 1\n- [ ] T1.1: add the greeting file\n- [ ] T1.2: fix the second task\n";

/// A plan that passes the plan gate (T9.1).
const VALID_PLAN: &str = "## File Operations\nCreate greeting.txt with hello.\n\n## Verification\nThe file contains hello.";

/// The builder session's script: it writes the task's file.
fn builder_steps() -> Vec<Step> {
    vec![Step::WriteFile {
        path: PathBuf::from("greeting.txt"),
        contents: "hello\n".into(),
    }]
}

fn scripted() -> MockProvider {
    MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "working".into(),
        }),
        Step::WriteFile {
            path: PathBuf::from("greeting.txt"),
            contents: "hello\n".into(),
        },
        Step::Event(AgentEvent::Result {
            text: "done".into(),
        }),
    ])
}

#[tokio::test]
async fn the_session_loop_builds_every_task_then_runs_one_discovery_round() {
    let fixture = Fixture::new(TASKS);
    let provider = scripted();
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();
    assert_eq!(attachment.snapshot.tasks.len(), 2);

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    assert!(matches!(
        events[0],
        EngineEvent::PhaseChanged {
            phase: Phase::Running
        }
    ));
    // Both pending tasks are built in one session, one after another.
    let started: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::TaskStarted { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(started, ["T1.1", "T1.2"]);
    for id in ["T1.1", "T1.2"] {
        assert!(events.iter().any(|e| matches!(
            e,
            EngineEvent::TaskFinished {
                id: finished,
                outcome: TaskOutcome::Done,
                commit: Some(_),
            } if finished == id
        )));
    }
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::Agent {
            event: AgentEvent::Result { .. }
        }
    )));

    let file = fixture.tasks_file();
    assert!(file.contains("- [x] T1.1: [--.B-] add the greeting file"));
    assert!(file.contains("- [x] T1.2: [--.B-] fix the second task"));
    let log = fixture.git(&["log", "--format=%s"]);
    assert_eq!(log.lines().next(), Some("feat(T1.2): fix the second task"));
    assert!(log.contains("feat(T1.1): add the greeting file"));
    let files = fixture.git(&["show", "--name-only", "--format=", "HEAD~1"]);
    assert!(
        files.contains("greeting.txt") && files.contains("TASKS.md"),
        "{files}"
    );

    let sessions = provider.sessions();
    // Two builds, then one discovery round on the empty queue.
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["T1.1", "T1.2", "discovery"]);
    assert!(sessions[0].prompt.contains("T1.1: add the greeting file"));
    assert!(sessions[0].allowed_tools.contains(&"Bash".to_string()));
    // The round found nothing to append, so the session ended there.
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Info && text == "No tasks added.")
    );
    assert!(
        fixture
            .data
            .path()
            .join("history")
            .read_dir()
            .unwrap()
            .count()
            == 3
    );
}

/// T103.1: sprint mode ends the session on the emptied queue -- no scheduled
/// discovery round starts, unlike the continuous mode above.
#[tokio::test]
async fn sprint_mode_skips_the_scheduled_discovery_round() {
    let fixture = Fixture::new(TASKS);
    let provider = scripted();
    let engine = fixture.engine_sprint(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // Both tasks ran; no round was announced.
    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["T1.1", "T1.2"]);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EngineEvent::DiscoveryChanged { discovering: true }))
    );
    assert!(
        !notices(&events)
            .iter()
            .any(|(_, text)| text == "No tasks added.")
    );
    // Two builder sessions in the history log, no third one.
    assert!(
        fixture
            .data
            .path()
            .join("history")
            .read_dir()
            .unwrap()
            .count()
            == 2
    );
}

#[tokio::test]
async fn a_failed_agent_commits_wip_and_leaves_the_task_pending() {
    let fixture = Fixture::new(TASKS);
    let provider = MockProvider::new(vec![Step::WriteFile {
        path: PathBuf::from("partial.txt"),
        contents: "half".into(),
    }])
    .ending_with(ExitKind::Failed);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Failed,
            ..
        }
    )));
    assert!(fixture.tasks_file().contains("- [ ] T1.1"));
    assert_eq!(
        fixture.git(&["log", "--format=%s", "-1"]).trim(),
        "WIP(T1.1): add the greeting file"
    );
}

#[tokio::test]
async fn a_plan_session_precedes_the_builder_and_feeds_the_plan_forward() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        vec![Step::Event(AgentEvent::Result {
            text: VALID_PLAN.into(),
        })],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_plan(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    // The plan session, the builder session, then the discovery round on the emptied queue.
    assert_eq!(labels, ["plan", "T1.1", "discovery"]);
    // The plan session is read-only and runs with the Plan agent's prompt and tools.
    assert!(
        sessions[0]
            .system_prompt
            .as_deref()
            .is_some_and(|p| p.contains("You are the Plan agent"))
    );
    assert_eq!(sessions[0].allowed_tools, ["Read", "Glob", "Grep"]);
    assert!(sessions[0].prompt.contains("T1.1: add the greeting file"));
    // The accepted plan reaches the builder prompt.
    assert!(sessions[1].prompt.contains("carry it out"));
    assert!(sessions[1].prompt.contains(VALID_PLAN));

    // The accepted plan is written to the data directory as plans/<task id>-<timestamp>.md.
    let plans_dir = fixture.data.path().join("plans");
    let files: Vec<_> = std::fs::read_dir(&plans_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(
        files[0].starts_with("T1.1-") && files[0].ends_with(".md"),
        "{files:?}"
    );
    assert_eq!(
        std::fs::read_to_string(plans_dir.join(&files[0])).unwrap(),
        VALID_PLAN
    );
    assert!(notices(&events)
        .iter()
        .any(|(level, text)| *level == NoticeLevel::Info && text.starts_with("Plan written to")));

    // The builder ran and the task completed as usual.
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            id,
            outcome: TaskOutcome::Done,
            commit: Some(_),
        } if id == "T1.1"
    )));
    assert!(
        fixture
            .tasks_file()
            .contains("- [x] T1.1: [-P.B-] add the greeting file")
    );
}

#[tokio::test]
async fn the_plan_falls_back_to_the_text_events_without_a_result() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        vec![
            Step::Event(AgentEvent::Text {
                text: "## File Operations\nCreate greeting.txt.".into(),
            }),
            Step::Event(AgentEvent::Text {
                text: "## Verification\nThe file contains hello.".into(),
            }),
        ],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_plan(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let _events = collect_until(&mut attachment.events, is_phase_startup).await;

    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["plan", "T1.1", "discovery"]);
    let plan =
        "## File Operations\nCreate greeting.txt.\n## Verification\nThe file contains hello.";
    assert!(sessions[1].prompt.contains(plan));
    let plans_dir = fixture.data.path().join("plans");
    let written: Vec<_> = std::fs::read_dir(&plans_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(
        std::fs::read_to_string(plans_dir.join(&written[0])).unwrap(),
        plan
    );
}

#[tokio::test]
async fn a_rejected_plan_is_retried_once_and_the_accepted_retry_builds() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        vec![Step::Event(AgentEvent::Result {
            text: "no sections at all".into(),
        })],
        vec![Step::Event(AgentEvent::Result {
            text: VALID_PLAN.into(),
        })],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_plan(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let _events = collect_until(&mut attachment.events, is_phase_startup).await;

    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["plan", "plan", "T1.1", "discovery"]);
    // The retry prompt carries the rejected plan and the rejection reason.
    assert!(sessions[1].prompt.contains("no sections at all"));
    assert!(sessions[1].prompt.contains("File Operations"));
    assert!(sessions[1].prompt.contains("Verification"));
    // The accepted retry is what the builder and the plan file see.
    assert!(sessions[2].prompt.contains(VALID_PLAN));
    assert!(
        fixture
            .tasks_file()
            .contains("- [x] T1.1: [-P.B-] add the greeting file")
    );
}

#[tokio::test]
async fn a_plan_rejected_twice_fails_the_task_without_a_builder_session() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        vec![Step::Event(AgentEvent::Result {
            text: "no sections at all".into(),
        })],
        vec![Step::Event(AgentEvent::Result {
            text: "still no sections".into(),
        })],
    ]);
    let engine = fixture.engine_with_plan(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // Two plan sessions, no builder session.
    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["plan", "plan"]);
    // The retry prompt names the rejected plan and the reason.
    assert!(sessions[1].prompt.contains("no sections at all"));
    assert!(sessions[1].prompt.contains("File Operations"));

    // The task is reported failed with no commit and stays pending; nothing was written.
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            id,
            outcome: TaskOutcome::Failed,
            commit: None,
        } if id == "T1.1"
    )));
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Error && text.contains("rejected twice"))
    );
    assert!(
        fixture
            .tasks_file()
            .contains("- [ ] T1.1: add the greeting file")
    );
    assert_eq!(fixture.git(&["log", "--format=%s", "-1"]).trim(), "initial");
    assert!(!fixture.data.path().join("plans").exists());
}

/// A Medium task's research session script: it returns one research report.
fn research_steps(report: &str) -> Vec<Step> {
    vec![Step::Event(AgentEvent::Result {
        text: report.into(),
    })]
}

/// A task description the complexity classifier does not mark Simple, so the
/// research stage runs even with the skip key on (T68.1).
const SUBSTANTIAL_TASK: &str = "- [ ] T1.1: implement the greeting widget parser\n";

#[tokio::test]
async fn a_research_session_precedes_the_plan_and_feeds_the_report_forward() {
    let fixture = Fixture::new(SUBSTANTIAL_TASK);
    let provider = MockProvider::per_session(vec![
        research_steps("1. What does the parser do today?\nIt parses nothing yet."),
        vec![Step::Event(AgentEvent::Result {
            text: VALID_PLAN.into(),
        })],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_plan(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    // The research session, the plan session, the builder session, then the
    // discovery round on the emptied queue.
    assert_eq!(labels, ["research", "plan", "T1.1", "discovery"]);
    // The research session runs with the Research agent's prompt and tools.
    assert!(
        sessions[0]
            .system_prompt
            .as_deref()
            .is_some_and(|p| p.contains("You are the Research agent"))
    );
    assert_eq!(sessions[0].allowed_tools, RESEARCH_TOOLS);
    assert!(
        sessions[0]
            .prompt
            .contains("T1.1: implement the greeting widget parser")
    );
    assert!(
        sessions[0]
            .prompt
            .contains("3 to 5 investigation questions")
    );
    // The report reaches the plan prompt as its prior artifact, and the plan
    // reaches the builder prompt as before.
    assert!(
        sessions[1]
            .prompt
            .contains("The research report for this task (a prior artifact, not a plan)")
    );
    assert!(
        sessions[1]
            .prompt
            .contains("1. What does the parser do today?\nIt parses nothing yet.")
    );
    assert!(sessions[2].prompt.contains(VALID_PLAN));
    // The builder prompt does not repeat the report: the planner read it.
    assert!(!sessions[2].prompt.contains("research report"));

    // The report is written to the data directory as research/<task id>-<timestamp>.md.
    let reports_dir = fixture.data.path().join("research");
    let files: Vec<_> = std::fs::read_dir(&reports_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(
        files[0].starts_with("T1.1-") && files[0].ends_with(".md"),
        "{files:?}"
    );
    assert_eq!(
        std::fs::read_to_string(reports_dir.join(&files[0])).unwrap(),
        "1. What does the parser do today?\nIt parses nothing yet."
    );
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Info
                && text.starts_with("Research report written to"))
    );

    // The research agent is announced before the planner takes over (T11.1),
    // and its raw stream lands in the history log like every other session.
    let agents: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::AgentChanged { agent, .. } => Some(agent.clone()),
            _ => None,
        })
        .collect();
    assert!(agents.iter().any(|a| a == "research"));
    assert!(
        agents.iter().position(|a| a == "research") < agents.iter().position(|a| a == "planner"),
        "{agents:?}"
    );

    // The RESEARCH tile is active while the session runs and done afterwards.
    let rail = rail(&events);
    assert!(
        rail.iter()
            .any(|state| state.stage_status(Stage::Research) == Some(TileStatus::Active))
    );
    assert!(
        rail.iter()
            .any(|state| state.stage_status(Stage::Research) == Some(TileStatus::Done))
    );

    // The task completed as usual.
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Done,
            ..
        }
    )));
}

#[tokio::test]
async fn the_report_reaches_the_builder_prompt_when_the_plan_stage_is_off() {
    let fixture = Fixture::new(SUBSTANTIAL_TASK);
    let provider = MockProvider::per_session(vec![
        research_steps("1. What does the parser do today?"),
        builder_steps(),
    ]);
    let engine = fixture.engine_with_research(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let _events = collect_until(&mut attachment.events, is_phase_startup).await;

    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    // No plan session: the report feeds the builder prompt directly.
    assert_eq!(labels, ["research", "T1.1", "discovery"]);
    assert!(
        sessions[1]
            .prompt
            .contains("The research report for this task (a prior artifact, not a plan)")
    );
    assert!(
        sessions[1]
            .prompt
            .contains("1. What does the parser do today?")
    );
    assert!(!sessions[1].prompt.contains("carry it out"));
}

#[tokio::test]
async fn a_fresh_report_on_disk_is_reused_without_a_new_session() {
    let fixture = Fixture::new(SUBSTANTIAL_TASK);
    // A report written moments ago: inside the ten-minute reuse window.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let dir = fixture.data.path().join("research");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("T1.1-{now}.md")), "1. Reused report.\n").unwrap();
    let provider = MockProvider::per_session(vec![
        vec![Step::Event(AgentEvent::Result {
            text: VALID_PLAN.into(),
        })],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_plan(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // No research session ran: the plan session is the first one, and the
    // reused report still reaches the plan prompt.
    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["plan", "T1.1", "discovery"]);
    assert!(sessions[0].prompt.contains("1. Reused report."));
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Info
                && text.contains("Reusing the research report for T1.1"))
    );
    // The reused report was not overwritten.
    assert_eq!(
        std::fs::read_to_string(dir.join(format!("T1.1-{now}.md"))).unwrap(),
        "1. Reused report.\n"
    );
}

#[tokio::test]
async fn a_stale_report_on_disk_is_not_reused() {
    let fixture = Fixture::new(SUBSTANTIAL_TASK);
    // A report twenty minutes old: outside the ten-minute reuse window.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let dir = fixture.data.path().join("research");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("T1.1-{}.md", now - 1200)),
        "1. Stale report.\n",
    )
    .unwrap();
    let provider = MockProvider::per_session(vec![
        research_steps("1. Fresh report."),
        vec![Step::Event(AgentEvent::Result {
            text: VALID_PLAN.into(),
        })],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_plan(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let _events = collect_until(&mut attachment.events, is_phase_startup).await;

    // A research session ran and its fresh report reaches the plan prompt.
    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["research", "plan", "T1.1", "discovery"]);
    assert!(sessions[1].prompt.contains("1. Fresh report."));
    assert!(!sessions[1].prompt.contains("Stale report"));
}

#[tokio::test]
async fn a_simple_task_skips_research_with_the_key_on_and_runs_it_when_off() {
    // "add the greeting file" is a Simple description, so the skip key decides.
    let tasks = "- [ ] T1.1: add the greeting file\n";

    let fixture = Fixture::new(tasks);
    let provider = MockProvider::per_session(vec![
        vec![Step::Event(AgentEvent::Result {
            text: VALID_PLAN.into(),
        })],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_plan(provider.clone());
    let mut attachment = engine.attach().unwrap();
    engine.start_build().unwrap();
    collect_until(&mut attachment.events, is_phase_startup).await;
    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    // The key is on by default: no research session and no report on disk.
    assert_eq!(labels, ["plan", "T1.1", "discovery"]);
    assert!(!fixture.data.path().join("research").exists());

    // With the key off, the same Simple task runs the research stage.
    let fixture = Fixture::new(tasks);
    let provider = MockProvider::per_session(vec![
        research_steps("1. What does the greeting file look like?"),
        vec![Step::Event(AgentEvent::Result {
            text: VALID_PLAN.into(),
        })],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_research(provider.clone());
    let mut attachment = engine.attach().unwrap();
    engine.start_build().unwrap();
    let _events = collect_until(&mut attachment.events, is_phase_startup).await;
    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    // With the key off the research stage runs; with the plan stage off the
    // report feeds the builder prompt.
    assert_eq!(labels, ["research", "T1.1", "discovery"]);
    assert!(
        sessions[1]
            .prompt
            .contains("1. What does the greeting file look like?")
    );
}

#[tokio::test]
async fn a_research_session_without_a_report_is_non_blocking() {
    let fixture = Fixture::new(SUBSTANTIAL_TASK);
    let provider = MockProvider::per_session(vec![
        // The research session completes but returns no report.
        vec![],
        vec![Step::Event(AgentEvent::Result {
            text: VALID_PLAN.into(),
        })],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_plan(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The pipeline continued to the plan and builder sessions, with a warning and no report file.
    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["research", "plan", "T1.1", "discovery"]);
    assert!(!sessions[1].prompt.contains("research report"));
    assert!(!fixture.data.path().join("research").exists());
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Warning
                && text.contains("research for T1.1"))
    );
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Done,
            ..
        }
    )));
}

#[tokio::test]
async fn an_interrupt_during_research_cancels_the_task() {
    let fixture = Fixture::new(SUBSTANTIAL_TASK);
    let provider = MockProvider::per_session(vec![
        vec![Step::Sleep(Duration::from_secs(2))],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_research(provider);
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    collect_until(
        &mut attachment.events,
        |event| matches!(event, EngineEvent::AgentChanged { agent, .. } if agent == "research"),
    )
    .await;
    engine.request_stop(true);
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The cancelled research session cancels the task: no plan, no build.
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Cancelled,
            ..
        }
    )));
    assert!(fixture.tasks_file().contains("- [ ] T1.1"));
}

#[tokio::test]
async fn the_active_agent_is_announced_and_replayed_for_a_reattaching_shell() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        vec![Step::Event(AgentEvent::Result {
            text: VALID_PLAN.into(),
        })],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_plan(provider);
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The planner is announced before the plan session's output, the builder between
    // the plan session and the builder session (T11.1); the discovery round that
    // follows on the emptied queue announces its own agent too (T42.1).
    let agents: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::AgentChanged { agent, .. } => Some(agent.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(agents, ["planner", "builder", "discovery"]);
    let at = |agent: &str| {
        events
            .iter()
            .position(|e| matches!(e, EngineEvent::AgentChanged { agent: a, .. } if a == agent))
            .expect("the agent was announced")
    };
    let plan_output = events
        .iter()
        .position(|e| {
            matches!(
                e,
                EngineEvent::Agent {
                    event: AgentEvent::Result { text }
                } if text.contains("File Operations")
            )
        })
        .expect("the plan session streamed output");
    let finished = events
        .iter()
        .position(|e| matches!(e, EngineEvent::TaskFinished { .. }))
        .expect("the task finished");
    assert!(at("planner") < plan_output);
    assert!(plan_output < at("builder") && at("builder") < finished);

    // Every announcement is replayed, so a reattaching shell shows the right agent.
    drop(attachment);
    let second = engine.attach().unwrap();
    for agent in ["planner", "builder", "discovery"] {
        assert!(second.snapshot.recent.iter().any(|e| matches!(
            e,
            EngineEvent::AgentChanged { agent: a, .. } if a == agent
        )));
    }
}

/// The agent-started events of a run: (agent, provider, model) per session start.
fn started(events: &[EngineEvent]) -> Vec<(String, String, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::AgentStarted {
                agent,
                provider,
                model,
            } => Some((agent.clone(), provider.clone(), model.clone())),
            _ => None,
        })
        .collect()
}

/// The agent-finished events of a run: (agent, outcome, duration_ms) per session end.
fn finished(events: &[EngineEvent]) -> Vec<(String, SessionOutcome, u64)> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::AgentFinished {
                agent,
                outcome,
                duration_ms,
            } => Some((agent.clone(), *outcome, *duration_ms)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_builder_session_end_carries_the_agent_type_and_duration_to_an_attached_shell() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    // The scripted sleep makes the engine-recorded duration measurably nonzero.
    let provider = MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "working".into(),
        }),
        Step::Sleep(Duration::from_millis(150)),
        Step::WriteFile {
            path: PathBuf::from("greeting.txt"),
            contents: "hello\n".into(),
        },
    ]);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The session's end carries the agent type, the outcome and the total duration.
    let (_, outcome, duration_ms) = finished(&events)
        .into_iter()
        .find(|(agent, _, _)| agent == "builder")
        .expect("the builder session pushed a finished event");
    assert_eq!(outcome, SessionOutcome::Finished);
    assert!(duration_ms >= 100, "duration_ms: {duration_ms}");
    // It lands after the last agent event of the session and before the task's
    // outcome line (the discovery round that follows streams its own output).
    let at = events
        .iter()
        .position(|e| {
            matches!(
                e,
                EngineEvent::AgentFinished { agent, .. } if agent == "builder"
            )
        })
        .expect("the finished event is in the stream");
    let last_agent = events[..at]
        .iter()
        .rposition(|e| matches!(e, EngineEvent::Agent { .. }))
        .expect("agent output streamed");
    let task = events
        .iter()
        .position(|e| matches!(e, EngineEvent::TaskFinished { .. }))
        .expect("the task finished");
    assert!(last_agent < at && at < task, "events: {events:#?}");

    // The finished event is replayed with the recent events, so a shell that
    // attached late still shows the line (T42.1) -- with its paired starting
    // line ahead of it (T78.1).
    drop(attachment);
    let second = engine.attach().unwrap();
    let recent = &second.snapshot.recent;
    let started_at = recent
        .iter()
        .position(|e| matches!(e, EngineEvent::AgentStarted { agent, .. } if agent == "builder"))
        .expect("the started event is replayed");
    let finished_at = recent
        .iter()
        .position(|e| {
            matches!(
                e,
                EngineEvent::AgentFinished {
                    agent,
                    outcome: SessionOutcome::Finished,
                    duration_ms,
                } if agent == "builder" && *duration_ms >= 100
            )
        })
        .expect("the finished event is replayed");
    assert!(started_at < finished_at, "recent: {recent:#?}");
}

#[tokio::test]
async fn planner_and_discovery_runs_push_their_own_finished_events() {
    // The append-tasks planner: one finished event before the planning end.
    let fixture = Fixture::new(TASKS);
    let after = format!("{TASKS}\n## Phase 2\n- [ ] T2.1: first added\n");
    let provider = planner_writing(&after);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();
    engine.start_add_tasks("add one thing").unwrap();
    let events = collect_until(&mut attachment.events, planning_finished).await;
    let ends = finished(&events);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].0, "planner");
    assert_eq!(ends[0].1, SessionOutcome::Finished);
    let planning_end = events
        .iter()
        .position(planning_finished)
        .expect("the planning run ended");
    assert!(
        events
            .iter()
            .position(
                |e| matches!(e, EngineEvent::AgentFinished { agent, .. } if agent == "planner")
            )
            .unwrap()
            < planning_end
    );
    drop(attachment);

    // The discovery round: its own announcement plus one finished event before
    // the discovery end (T42.1).
    let after = format!("{TASKS}\n- [ ] D1.1: fix the greeting\n");
    let provider = MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "scanning the project".into(),
        }),
        Step::WriteFile {
            path: PathBuf::from("TASKS.md"),
            contents: after.clone(),
        },
    ]);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();
    engine.start_discovery().unwrap();
    let events = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::DiscoveryChanged { discovering: false })
    })
    .await;
    let announced = events
        .iter()
        .position(|e| matches!(e, EngineEvent::AgentChanged { agent, .. } if agent == "discovery"))
        .expect("the discovery agent is announced");
    let first_agent_event = events
        .iter()
        .position(|e| matches!(e, EngineEvent::Agent { .. }))
        .expect("discovery output arrives");
    assert!(announced < first_agent_event, "events: {events:#?}");
    let ends = finished(&events);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].0, "discovery");
    assert_eq!(ends[0].1, SessionOutcome::Finished);
    let discovery_end = events
        .iter()
        .position(|e| matches!(e, EngineEvent::DiscoveryChanged { discovering: false }))
        .unwrap();
    assert!(
        events
            .iter()
            .position(
                |e| matches!(e, EngineEvent::AgentFinished { agent, .. } if agent == "discovery")
            )
            .unwrap()
            < discovery_end
    );
}

/// One run's starting event: exactly one `AgentStarted` for `agent`, named with
/// the provider and model, positioned before the run's first agent output and
/// before its matching finished event (T78.1).
fn assert_one_starting_line(
    events: &[EngineEvent],
    agent: &str,
    provider: &str,
    model: Option<&str>,
) -> usize {
    let starts: Vec<_> = started(events)
        .into_iter()
        .filter(|(a, _, _)| a == agent)
        .collect();
    assert_eq!(starts.len(), 1, "events: {events:#?}");
    assert_eq!(
        starts[0],
        (
            agent.to_string(),
            provider.to_string(),
            model.map(str::to_string)
        ),
        "events: {events:#?}"
    );
    let at = events
        .iter()
        .position(|e| {
            matches!(
                e,
                EngineEvent::AgentStarted { agent: a, .. } if a == agent
            )
        })
        .expect("the starting event is in the stream");
    // The starting line lands before any output of the run, so a shell shows
    // it the moment the run begins.
    assert!(
        !events[..at]
            .iter()
            .any(|e| matches!(e, EngineEvent::Agent { .. })),
        "output precedes the starting line: {events:#?}"
    );
    assert!(
        events[at..]
            .iter()
            .any(|e| matches!(e, EngineEvent::AgentFinished { agent: a, .. } if a == agent)),
        "the run's finished event follows: {events:#?}"
    );
    at
}

#[tokio::test]
async fn agent_runs_push_their_starting_event_before_any_output() {
    // The builder run: one starting line naming the agent, the provider and no
    // model (the engine has none configured), before the session's output.
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "working".into(),
        }),
        Step::WriteFile {
            path: PathBuf::from("greeting.txt"),
            contents: "hello\n".into(),
        },
    ]);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();
    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;
    let at = assert_one_starting_line(&events, "builder", "mock", None);
    // The pair: the starting line precedes the existing finished line (T78.1).
    let finished_at = events
        .iter()
        .position(|e| {
            matches!(
                e,
                EngineEvent::AgentFinished { agent, .. } if agent == "builder"
            )
        })
        .expect("the builder session pushed a finished event");
    assert!(at < finished_at, "events: {events:#?}");
    drop(attachment);

    // The starting event is replayed with the recent events, so a shell that
    // attached late still shows the line paired with the finished line.
    let second = engine.attach().unwrap();
    assert!(second.snapshot.recent.iter().any(|e| matches!(
        e,
        EngineEvent::AgentStarted {
            agent,
            provider,
            model,
        } if agent == "builder" && provider == "mock" && model.is_none()
    )));

    // The append-tasks planner run: its starting line lands before the
    // planning flag flips and before any planner output.
    let fixture = Fixture::new(TASKS);
    let after = format!("{TASKS}\n## Phase 2\n- [ ] T2.1: first added\n");
    let provider = planner_writing(&after);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();
    engine.start_add_tasks("add one thing").unwrap();
    let events = collect_until(&mut attachment.events, planning_finished).await;
    let at = assert_one_starting_line(&events, "planner", "mock", None);
    assert!(
        at < events
            .iter()
            .position(|e| matches!(e, EngineEvent::PlanningChanged { planning: true }))
            .expect("the planning flag flipped"),
        "events: {events:#?}"
    );
    drop(attachment);

    // The discovery round: its starting line lands before its output.
    let after = format!("{TASKS}\n- [ ] D1.1: fix the greeting\n");
    let provider = MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "scanning the project".into(),
        }),
        Step::WriteFile {
            path: PathBuf::from("TASKS.md"),
            contents: after.clone(),
        },
    ]);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();
    engine.start_discovery().unwrap();
    let events = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::DiscoveryChanged { discovering: false })
    })
    .await;
    assert_one_starting_line(&events, "discovery", "mock", None);
}

#[tokio::test]
async fn a_configured_model_names_the_model_in_the_starting_event() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::new(builder_steps());
    let engine = fixture.engine_configured(provider, "model = \"mistral-small\"\n");
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The configured model rides along with the provider the run uses; the
    // resolver hands back the mock whatever the configured kind (T78.1).
    assert_one_starting_line(&events, "builder", "mock", Some("mistral-small"));
}

#[tokio::test]
async fn a_failed_run_still_leaves_its_starting_line() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::new(vec![Step::WriteFile {
        path: PathBuf::from("partial.txt"),
        contents: "half".into(),
    }])
    .ending_with(ExitKind::Failed);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The run failed, so the finished line says so; the starting line is still
    // there, first, with no agent output in between (T78.1).
    let at = assert_one_starting_line(&events, "builder", "mock", None);
    let finished_at = events
        .iter()
        .position(|e| {
            matches!(
                e,
                EngineEvent::AgentFinished {
                    agent,
                    outcome: SessionOutcome::Failed,
                    ..
                } if agent == "builder"
            )
        })
        .expect("the failed session pushed a finished event");
    assert!(at < finished_at, "events: {events:#?}");
    assert!(
        !events[at..finished_at]
            .iter()
            .any(|e| matches!(e, EngineEvent::Agent { .. })),
        "events: {events:#?}"
    );
}

#[tokio::test]
async fn each_plan_stage_attempt_pushes_its_own_finished_event() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        vec![Step::Event(AgentEvent::Result {
            text: "no sections at all".into(),
        })],
        vec![Step::Event(AgentEvent::Result {
            text: VALID_PLAN.into(),
        })],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_plan(provider);
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The rejected first attempt and the accepted retry each leave their own line,
    // then the builder session its own (T42.1); each finished line is preceded by
    // its own starting line (T78.1).
    let ends = finished(&events);
    let planner: Vec<_> = ends
        .iter()
        .filter(|(agent, _, _)| agent == "planner")
        .cloned()
        .collect();
    assert_eq!(planner.len(), 2);
    assert!(
        planner
            .iter()
            .all(|(_, outcome, _)| *outcome == SessionOutcome::Finished)
    );
    assert_eq!(
        ends.iter()
            .filter(|(agent, _, _)| agent == "builder")
            .count(),
        1
    );
    // Two planner starting lines interleave with the attempts: each attempt's
    // starting line precedes its own finished line (T78.1).
    let starts: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(
                e,
                EngineEvent::AgentStarted { agent, .. } if agent == "planner"
            )
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(starts.len(), 2, "events: {events:#?}");
    let planner_finishes: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(
                e,
                EngineEvent::AgentFinished { agent, .. } if agent == "planner"
            )
        })
        .map(|(i, _)| i)
        .collect();
    assert_eq!(planner_finishes.len(), 2, "events: {events:#?}");
    assert!(
        starts[0] < planner_finishes[0]
            && planner_finishes[0] < starts[1]
            && starts[1] < planner_finishes[1],
        "events: {events:#?}"
    );
    // The builder's own starting line precedes its finished line too.
    let builder_start = events
        .iter()
        .position(|e| {
            matches!(
                e,
                EngineEvent::AgentStarted { agent, .. } if agent == "builder"
            )
        })
        .expect("the builder session pushed a starting event");
    let builder_finish = events
        .iter()
        .position(|e| {
            matches!(
                e,
                EngineEvent::AgentFinished { agent, .. } if agent == "builder"
            )
        })
        .expect("the builder session pushed a finished event");
    assert!(builder_start < builder_finish, "events: {events:#?}");
}

#[tokio::test]
async fn failed_and_cancelled_builder_sessions_still_push_the_duration() {
    // A failed session: the finished event carries the failed wording and its duration.
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::new(vec![Step::WriteFile {
        path: PathBuf::from("partial.txt"),
        contents: "half".into(),
    }])
    .ending_with(ExitKind::Failed);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();
    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;
    let ends = finished(&events);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].0, "builder");
    assert_eq!(ends[0].1, SessionOutcome::Failed);
    drop(attachment);

    // An interrupted session: the cancelled wording with the same duration.
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::new(vec![Step::Sleep(Duration::from_secs(60))]);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();
    engine.start_build().unwrap();
    engine.request_stop(true);
    let events = collect_until(&mut attachment.events, is_phase_startup).await;
    let ends = finished(&events);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].0, "builder");
    assert_eq!(ends[0].1, SessionOutcome::Cancelled);
}

#[tokio::test]
async fn start_build_is_rejected_when_busy_or_nothing_is_pending() {
    let fixture = Fixture::new("- [ ] T1.1: fix the only task\n");
    let provider = MockProvider::new(vec![Step::Sleep(Duration::from_millis(300))]);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    assert_eq!(
        engine.start_build().unwrap_err(),
        "a build is already running"
    );
    collect_until(&mut attachment.events, is_phase_startup).await;

    assert_eq!(
        engine.start_build().unwrap_err(),
        "no pending tasks in TASKS.md"
    );
}

/// T98.1: while a build runs, removing the current (in-progress) task is
/// refused with a reason and the line stays, while a pending task is
/// removed from the file and the queue reconciles immediately.
#[tokio::test]
async fn remove_task_refuses_the_running_task_and_removes_a_pending_one_mid_build() {
    let fixture = Fixture::new(TASKS);
    let provider = MockProvider::new(vec![Step::Sleep(Duration::from_secs(60))]);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::TaskStarted { .. })
    })
    .await;

    // The running (in-progress) task is refused with a reason; its line
    // stays in the file whatever shape the session has given it so far.
    assert_eq!(
        engine.remove_task("T1.1").await.unwrap_err(),
        "task T1.1 is in progress; wait for it to finish before removing it"
    );
    let tasks = fixture.tasks_file();
    assert!(tasks.contains("T1.1"));
    assert!(tasks.contains("add the greeting file"));

    // A pending task is removed even mid-build, and the queue reconciles
    // right away: the TasksChanged broadcast no longer carries T1.2.
    engine.remove_task("T1.2").await.unwrap();
    assert!(!fixture.tasks_file().contains("T1.2"));
    let is_tasks_changed = |e: &EngineEvent| matches!(e, EngineEvent::TasksChanged { .. });
    let events = collect_until(&mut attachment.events, is_tasks_changed).await;
    let changed = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::TasksChanged { tasks } => Some(tasks),
            _ => None,
        })
        .next_back()
        .unwrap();
    assert!(!changed.iter().any(|t| t.id == "T1.2"));
    assert!(changed.iter().any(|t| t.id == "T1.1"));

    // Tearing down: the interrupted build ends cancelled and the file keeps
    // the refused task unchecked.
    engine.request_stop(true);
    collect_until(&mut attachment.events, is_phase_startup).await;
    assert!(
        fixture
            .tasks_file()
            .contains("- [ ] T1.1: add the greeting file")
    );
}

#[tokio::test]
async fn stop_now_cancels_the_agent_without_committing() {
    let fixture = Fixture::new(TASKS);
    let provider = MockProvider::new(vec![Step::Sleep(Duration::from_secs(60))]);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    engine.request_stop(true);
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Cancelled,
            commit: None,
            ..
        }
    )));
    assert_eq!(fixture.git(&["log", "--format=%s", "-1"]).trim(), "initial");
    // The cancelled task was never finished: it stays unchecked in the task file.
    let tasks = std::fs::read_to_string(fixture.path().join("TASKS.md")).unwrap();
    assert!(tasks.contains("- [ ] T1.1: add the greeting file"));
    // The interrupted session was the only agent session: no second task, no
    // discovery round, because `stopping` is set.
    assert_eq!(provider.sessions().len(), 1);
    assert_eq!(
        engine.start_build().unwrap_err(),
        "the engine is shutting down"
    );
}

#[tokio::test]
async fn a_soft_stop_finishes_the_task_stops_the_loop_and_keeps_the_engine_serving() {
    let fixture = Fixture::new(TASKS);
    let provider = scripted();
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    engine.request_stop(false);
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The running task finished normally: done and committed as usual.
    let started: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::TaskStarted { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(started, ["T1.1"]);
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            id,
            outcome: TaskOutcome::Done,
            commit: Some(_),
        } if id == "T1.1"
    )));
    assert!(
        fixture
            .tasks_file()
            .contains("- [x] T1.1: [--.B-] add the greeting file")
    );
    assert!(
        fixture
            .tasks_file()
            .contains("- [ ] T1.2: fix the second task")
    );
    assert_eq!(
        fixture.git(&["log", "--format=%s", "-1"]).trim(),
        "feat(T1.1): add the greeting file"
    );
    // No second task and no discovery round: the loop stopped.
    assert_eq!(provider.sessions().len(), 1);

    // A soft stop is a stop, not a quit: the process stand-in (the shutdown
    // token) is intact and the engine is idle.
    assert!(!engine.shutdown_token().is_cancelled());
    assert!(!engine.is_running());

    // While the stop is still held, new work is refused.
    assert_eq!(
        engine.start_build().unwrap_err(),
        "the engine is shutting down"
    );

    // Released by the shutdown handshake: commands work again without
    // restarting anything, and the next task runs to done.
    engine.end_stop();
    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            id,
            outcome: TaskOutcome::Done,
            commit: Some(_),
        } if id == "T1.2"
    )));
    assert!(
        fixture
            .tasks_file()
            .contains("- [x] T1.2: [--.B-] fix the second task")
    );
}

#[tokio::test]
async fn an_interrupt_cancels_the_running_task_and_keeps_the_engine_serving() {
    let fixture = Fixture::new(TASKS);
    // Every session sleeps first, so the interrupt lands mid-task.
    let provider = MockProvider::new(vec![
        Step::Sleep(Duration::from_millis(400)),
        Step::Event(AgentEvent::Result {
            text: "done".into(),
        }),
    ]);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    engine.request_stop(true);
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The running task was cancelled, not finished: no commit, no check mark.
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            id,
            outcome: TaskOutcome::Cancelled,
            commit: None,
        } if id == "T1.1"
    )));
    assert!(
        fixture
            .tasks_file()
            .contains("- [ ] T1.1: add the greeting file")
    );
    assert_eq!(fixture.git(&["log", "--format=%s", "-1"]).trim(), "initial");
    // The interrupted session was the only agent session: no second task and no
    // discovery round started.
    assert_eq!(provider.sessions().len(), 1);

    // An interrupt is a stop, not a quit: the engine is idle and the process
    // stand-in (the shutdown token) is intact.
    assert!(!engine.shutdown_token().is_cancelled());
    assert!(!engine.is_running());

    // While the stop is still held, new work is refused.
    assert_eq!(
        engine.start_build().unwrap_err(),
        "the engine is shutting down"
    );

    // Released by the shutdown handshake: commands work again without
    // restarting anything, and the next task runs to done.
    engine.end_stop();
    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            id,
            outcome: TaskOutcome::Done,
            commit: Some(_),
        } if id == "T1.2"
    )));
    assert!(
        fixture
            .tasks_file()
            .contains("- [x] T1.2: [--.B-] fix the second task")
    );

    // AddTasks and RunDiscovery are answered again too: both sessions run
    // (they change nothing with this script) and the engine stays serving.
    engine.start_add_tasks("more work").unwrap();
    collect_until(&mut attachment.events, planning_finished).await;
    engine.start_discovery().unwrap();
    collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::DiscoveryChanged { discovering: false })
    })
    .await;
    assert!(!engine.shutdown_token().is_cancelled());
}

#[tokio::test]
async fn a_cancelled_soft_stop_continues_the_loop_with_the_next_task() {
    let fixture = Fixture::new(TASKS);
    // Every session sleeps first, so the cancel lands mid-task.
    let provider = MockProvider::new(vec![
        Step::Sleep(Duration::from_millis(400)),
        Step::Event(AgentEvent::Result {
            text: "done".into(),
        }),
    ]);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    engine.request_stop(false);
    engine.cancel_soft_stop().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The cancel reached the attached shell as an info notice.
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::Notice { level: NoticeLevel::Info, text }
            if text.contains("Soft stop cancelled")
    )));

    // The loop continued with the next pending task after the running one.
    let started: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::TaskStarted { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(started, ["T1.1", "T1.2"]);
    for id in ["T1.1", "T1.2"] {
        assert!(events.iter().any(|e| matches!(
            e,
            EngineEvent::TaskFinished {
                id: finished,
                outcome: TaskOutcome::Done,
                commit: Some(_),
            } if finished == id
        )));
        assert!(fixture.tasks_file().contains(&format!("- [x] {id}")));
    }

    // Nothing is stopping anymore: the engine is idle, keeps serving, and a
    // second cancel is a command error.
    assert!(!engine.shutdown_token().is_cancelled());
    assert!(!engine.is_running());
    assert_eq!(
        engine.cancel_soft_stop().unwrap_err(),
        "no soft stop is pending"
    );
}

#[tokio::test]
async fn a_cancel_soft_stop_without_a_pending_stop_is_a_command_error() {
    let fixture = Fixture::new(TASKS);
    let engine = fixture.engine(MockProvider::new(vec![Step::Sleep(Duration::from_secs(
        60,
    ))]));
    let mut attachment = engine.attach().unwrap();

    // Idle: no stop of any kind is pending.
    assert_eq!(
        engine.cancel_soft_stop().unwrap_err(),
        "no soft stop is pending"
    );

    // A NOW stop is not a soft stop: it cannot be cancelled.
    engine.start_build().unwrap();
    engine.request_stop(true);
    assert_eq!(
        engine.cancel_soft_stop().unwrap_err(),
        "no soft stop is pending"
    );
    collect_until(&mut attachment.events, is_phase_startup).await;

    // After the session ends, still nothing pending.
    assert_eq!(
        engine.cancel_soft_stop().unwrap_err(),
        "no soft stop is pending"
    );
}

#[tokio::test]
async fn a_second_shell_is_rejected_until_the_first_detaches_and_sees_replayed_output() {
    let fixture = Fixture::new("- [ ] T1.1: fix the only task\n");
    let engine = fixture.engine(scripted());
    let mut first = engine.attach().unwrap();
    let error = engine.attach().err().unwrap().to_string();
    assert!(
        error.contains("already attached") && error.contains(&fixture.path().display().to_string())
    );

    engine.start_build().unwrap();
    collect_until(&mut first.events, is_phase_startup).await;
    drop(first);

    let second = engine.attach().unwrap();
    assert_eq!(second.snapshot.phase, Phase::Startup);
    assert!(second.snapshot.tasks[0].done);
    assert!(second.snapshot.recent.iter().any(|e| matches!(
        e,
        EngineEvent::Agent {
            event: AgentEvent::Result { .. }
        }
    )));
}

#[tokio::test]
async fn an_edited_task_file_is_picked_up_and_broadcast() {
    let fixture = Fixture::new("- [ ] T1.1: first\n");
    let engine = fixture.engine(MockProvider::new(vec![]));
    let mut attachment = engine.attach().unwrap();
    engine.spawn_task_file_watch();

    std::fs::write(
        fixture.path().join("TASKS.md"),
        "- [ ] T1.1: first\n- [ ] T1.2: second\n",
    )
    .unwrap();
    let seen = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::TasksChanged { .. })
    })
    .await;
    let Some(EngineEvent::TasksChanged { tasks }) = seen.last() else {
        unreachable!()
    };
    let ids: Vec<_> = tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["T1.1", "T1.2"]);
    engine.shutdown_token().cancel();
}

#[tokio::test]
async fn startup_cleanup_prunes_old_completed_tasks_and_broadcasts_the_list() {
    // Groups 1 to 4 are finished (seven checked lines), group 5 is pending
    // and last: the two oldest checked lines go, the newest five stay.
    let seed = "# Tasks\n\n## Phase 1\n- [x] T1.1: old one\n- [x] T1.2: old two\n\
                ## Phase 2\n- [x] T2.1: old three\n- [x] T2.2: old four\n- [x] T3.1: old five\n\
                ## Phase 3\n- [x] T4.1: old six\n- [x] T4.2: old seven\n\
                ## Phase 4\n- [ ] T5.1: still to do\n";
    let fixture = Fixture::new(seed);
    let engine = fixture.engine(MockProvider::new(vec![]));
    let mut attachment = engine.attach().unwrap();

    engine.cleanup_completed_tasks().await;

    let seen = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::TasksChanged { .. })
    })
    .await;
    let Some(EngineEvent::TasksChanged { tasks }) = seen.last() else {
        unreachable!()
    };
    // The queue matches the cleaned file.
    let ids: Vec<_> = tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["T2.1", "T2.2", "T3.1", "T4.1", "T4.2", "T5.1"]);
    // The file on disk: only the newest five completed lines plus the group
    // headers and the pending tail group, every kept line byte-identical.
    // Phase 1 lost all its task lines, so its heading goes too (T79.1): no
    // dangling heading over an empty section.
    let file = fixture.tasks_file();
    assert_eq!(
        file,
        "# Tasks\n\n## Phase 2\n- [x] T2.1: old three\n- [x] T2.2: old four\n- [x] T3.1: old five\n\
          ## Phase 3\n- [x] T4.1: old six\n- [x] T4.2: old seven\n\
          ## Phase 4\n- [ ] T5.1: still to do\n"
    );
    // A second run on the cleaned file rewrites nothing and emits no event.
    engine.cleanup_completed_tasks().await;
    assert!(try_recv_all(&mut attachment.events).is_empty());
    assert_eq!(file, fixture.tasks_file());
    engine.shutdown_token().cancel();
}

/// Drains every event already buffered in `rx` without waiting.
fn try_recv_all(rx: &mut tokio::sync::broadcast::Receiver<EngineEvent>) -> Vec<EngineEvent> {
    let mut drained = vec![];
    while let Ok(event) = rx.try_recv() {
        drained.push(event);
    }
    drained
}

#[tokio::test]
async fn existing_h_and_d_lines_survive_parsing_and_an_external_edit_reconciles() {
    let tasks = "# Tasks\n- [x] H1.1: human task\n- [ ] D1.1: discovery\n- [ ] T1.1: build\n";
    let fixture = Fixture::new(tasks);
    let engine = fixture.engine(MockProvider::new(vec![]));
    let mut attachment = engine.attach().unwrap();
    engine.spawn_task_file_watch();

    // An external edit appending a task leaves the existing H and D lines alone.
    std::fs::write(
        fixture.path().join("TASKS.md"),
        format!("{tasks}- [ ] T1.2: added externally\n"),
    )
    .unwrap();
    let seen = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::TasksChanged { .. })
    })
    .await;
    let Some(EngineEvent::TasksChanged { tasks }) = seen.last() else {
        unreachable!()
    };
    let view: Vec<_> = tasks
        .iter()
        .map(|t| (t.id.as_str(), t.origin, t.done, t.is_malformed()))
        .collect();
    assert_eq!(
        view,
        [
            ("H1.1", None, true, false),
            ("D1.1", Some('D'), false, false),
            ("T1.1", Some('T'), false, false),
            ("T1.2", Some('T'), false, false),
        ]
    );
    engine.shutdown_token().cancel();
}

fn planning_finished(event: &EngineEvent) -> bool {
    matches!(event, EngineEvent::PlanningChanged { planning: false })
}

fn notices(events: &[EngineEvent]) -> Vec<(NoticeLevel, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::Notice { level, text } => Some((*level, text.clone())),
            _ => None,
        })
        .collect()
}

fn planner_writing(contents: &str) -> MockProvider {
    MockProvider::new(vec![Step::WriteFile {
        path: PathBuf::from("TASKS.md"),
        contents: contents.into(),
    }])
}

#[tokio::test]
async fn a_planner_session_appends_valid_tasks_and_the_queue_reconciles() {
    let fixture = Fixture::new(TASKS);
    let provider = MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "expanding the request".into(),
        }),
        Step::WriteFile {
            path: PathBuf::from("TASKS.md"),
            contents: format!(
                "{TASKS}\n## Phase 2\n- [ ] T2.1: first added\n- [ ] T2.2: second added\n"
            ),
        },
    ]);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_add_tasks("  add two things  ").unwrap();
    let events = collect_until(&mut attachment.events, planning_finished).await;

    // The starting line precedes the announcement, which comes before the
    // planning flag flips (T24.1, T78.1), so events[0] is the starting line,
    // events[1] the announcement and events[2] the flag change.
    assert!(matches!(
        &events[0],
        EngineEvent::AgentStarted { agent, .. } if agent == "planner"
    ));
    assert!(matches!(
        &events[1],
        EngineEvent::AgentChanged { agent, .. } if agent == "planner"
    ));
    assert!(matches!(
        events[2],
        EngineEvent::PlanningChanged { planning: true }
    ));
    // The announcement precedes the planner's output, so the shell's output
    // frame carries the planner title.
    let announced = events
        .iter()
        .position(|e| matches!(e, EngineEvent::AgentChanged { agent, .. } if agent == "planner"))
        .expect("the planner agent is announced");
    let first_agent_event = events
        .iter()
        .position(|e| matches!(e, EngineEvent::Agent { .. }))
        .expect("planner output arrives");
    assert!(announced < first_agent_event, "events: {events:#?}");
    assert_eq!(
        notices(&events),
        [(NoticeLevel::Info, "2 tasks added.".to_string())]
    );
    let Some(EngineEvent::TasksChanged { tasks }) = events
        .iter()
        .rev()
        .find(|e| matches!(e, EngineEvent::TasksChanged { .. }))
    else {
        panic!("no TasksChanged in {events:#?}");
    };
    let ids: Vec<_> = tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["T1.1", "T1.2", "T2.1", "T2.2"]);
    assert!(tasks.iter().all(|t| !t.done));

    // The engine remembers the appended IDs for the discovery cooldown.
    assert_eq!(engine.ui_added_task_ids(), ["T2.1", "T2.2"]);
    assert!(engine.is_ui_added("T2.1"));
    assert!(!engine.is_ui_added("T1.1"));

    let sessions = provider.sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].label, "planner");
    assert!(sessions[0].prompt.contains("add two things"));
}

#[tokio::test]
async fn a_planner_may_append_t_tasks_to_a_file_with_h_and_d_lines() {
    let tasks = "# Tasks\n- [x] H1.1: human task\n- [ ] D1.1: discovery\n- [ ] T1.1: build\n";
    let fixture = Fixture::new(tasks);
    let after = format!("{tasks}\n## Phase 2\n- [ ] T2.1: added\n");
    let engine = fixture.engine(planner_writing(&after));
    let mut attachment = engine.attach().unwrap();

    engine.start_add_tasks("one more").unwrap();
    let events = collect_until(&mut attachment.events, planning_finished).await;

    assert_eq!(
        notices(&events),
        [(NoticeLevel::Info, "1 task added.".to_string())]
    );
    let file = fixture.tasks_file();
    assert_eq!(file, after);
    assert!(file.contains("- [x] H1.1: human task\n- [ ] D1.1: discovery\n"));
    let Some(EngineEvent::TasksChanged { tasks }) = events
        .iter()
        .rev()
        .find(|e| matches!(e, EngineEvent::TasksChanged { .. }))
    else {
        panic!("no TasksChanged in {events:#?}");
    };
    let ids: Vec<_> = tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["H1.1", "D1.1", "T1.1", "T2.1"]);
}

#[tokio::test]
async fn an_invalid_planner_result_is_rolled_back_with_an_error() {
    let invalid = [
        // Modifies an existing task.
        TASKS.replace("- [ ] T1.1: add", "- [x] T1.1: add"),
        // Appends a task with a malformed ID.
        format!("{TASKS}- [ ] no id here\n"),
        // Appends non-T tasks: an H task, a D task and a bare-number ID.
        format!("{TASKS}- [ ] H2.1: human\n"),
        format!("{TASKS}- [ ] D2.1: discovery\n"),
        format!("{TASKS}- [ ] 2.1: bare number\n"),
    ];
    for after in invalid {
        let fixture = Fixture::new(TASKS);
        let engine = fixture.engine(planner_writing(&after));
        let mut attachment = engine.attach().unwrap();

        engine.start_add_tasks("do something").unwrap();
        let events = collect_until(&mut attachment.events, planning_finished).await;

        let notices = notices(&events);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].0, NoticeLevel::Error);
        assert!(
            notices[0].1.contains("planner result rejected") && notices[0].1.contains("restored"),
            "{}",
            notices[0].1
        );
        assert_eq!(fixture.tasks_file(), TASKS);
        // A rejected result records no UI-added IDs.
        assert!(engine.ui_added_task_ids().is_empty());
    }
}

#[tokio::test]
async fn a_busy_engine_refuses_the_planner_request() {
    let fixture = Fixture::new(TASKS);
    let engine = fixture.engine(MockProvider::new(vec![Step::Sleep(Duration::from_millis(
        400,
    ))]));
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    assert_eq!(
        engine.start_add_tasks("more").unwrap_err(),
        "a build is running; wait for it to finish"
    );
    collect_until(&mut attachment.events, is_phase_startup).await;

    let planner = fixture.engine(MockProvider::new(vec![Step::Sleep(Duration::from_millis(
        400,
    ))]));
    let mut attachment = planner.attach().unwrap();
    planner.start_add_tasks("first").unwrap();
    assert_eq!(
        planner.start_add_tasks("second").unwrap_err(),
        "the planner is already running"
    );
    assert!(
        planner.start_build().is_err(),
        "a build is refused while planning"
    );
    assert_eq!(
        planner.start_add_tasks("  ").unwrap_err(),
        "the request is empty"
    );
    collect_until(&mut attachment.events, planning_finished).await;
}

#[tokio::test]
async fn a_missing_task_file_is_created_empty_before_planning() {
    let fixture = Fixture::new(TASKS);
    std::fs::remove_file(fixture.path().join("TASKS.md")).unwrap();
    let engine = fixture.engine(MockProvider::new(vec![Step::Sleep(Duration::from_millis(
        100,
    ))]));
    let mut attachment = engine.attach().unwrap();

    engine.start_add_tasks("start a list").unwrap();
    assert_eq!(fixture.tasks_file(), "# Tasks\n");
    let events = collect_until(&mut attachment.events, planning_finished).await;

    assert_eq!(
        notices(&events),
        [(NoticeLevel::Info, "No tasks added.".to_string())]
    );
    assert_eq!(fixture.tasks_file(), "# Tasks\n");
}

fn discovery_writing(contents: &str) -> MockProvider {
    MockProvider::new(vec![Step::WriteFile {
        path: PathBuf::from("TASKS.md"),
        contents: contents.into(),
    }])
}

/// A research queue-creation session: it appends `contents` to the task file
/// and returns `report` as its final message.
fn queue_research_writing(contents: &str, report: &str) -> MockProvider {
    MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "investigating the project".into(),
        }),
        Step::WriteFile {
            path: PathBuf::from("TASKS.md"),
            contents: contents.into(),
        },
        Step::Event(AgentEvent::Result {
            text: report.into(),
        }),
    ])
}

/// The queue-creation run's event prologue: the starting line (T78.1) and the
/// research agent's announcement come before the planning flag flips, so the
/// shell labels the run (T24.1, T69.1).
fn assert_research_run_announced(events: &[EngineEvent]) {
    assert!(matches!(
        &events[0],
        EngineEvent::AgentStarted { agent, .. } if agent == "research"
    ));
    assert!(matches!(
        &events[1],
        EngineEvent::AgentChanged { agent, .. } if agent == "research"
    ));
    assert!(matches!(
        events[2],
        EngineEvent::PlanningChanged { planning: true }
    ));
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::AgentFinished { agent, outcome: SessionOutcome::Finished, .. } if agent == "research"
    )));
}

/// The newest research report saved under `stem` in the data directory.
fn research_report_file(fixture: &Fixture, stem: &str) -> String {
    let dir = fixture.data.path().join("research");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&format!("{stem}-")) && name.ends_with(".md"))
        .collect();
    assert!(!files.is_empty(), "no {stem} report saved");
    files.sort();
    let newest = files.pop().unwrap();
    std::fs::read_to_string(dir.join(newest)).unwrap()
}

#[tokio::test]
async fn a_bootstrap_queue_creation_run_is_a_research_session_whose_tasks_validate() {
    let fixture = Fixture::new(TASKS);
    let provider = queue_research_writing(
        &format!("{TASKS}\n## Phase 2\n- [ ] T2.1: first added\n- [ ] T2.2: second added\n"),
        "1. What does the project build?\nIt builds the greeting file.",
    );
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine
        .start_queue_creation(QueueCreation::Bootstrap)
        .unwrap();
    let events = collect_until(&mut attachment.events, planning_finished).await;

    // The run announces the research agent before the planning flag flips.
    assert_research_run_announced(&events);
    // The appended tasks pass the existing append-tasks validation.
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Info && text == "2 tasks added.")
    );
    // The queue reconciles with the appended tasks.
    let Some(EngineEvent::TasksChanged { tasks }) = events
        .iter()
        .rev()
        .find(|e| matches!(e, EngineEvent::TasksChanged { .. }))
    else {
        panic!("no TasksChanged in {events:#?}");
    };
    let ids: Vec<_> = tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["T1.1", "T1.2", "T2.1", "T2.2"]);
    // The appended IDs count as UI-added like the planner's.
    assert_eq!(engine.ui_added_task_ids(), ["T2.1", "T2.2"]);

    // It is a research session: the label, the system prompt and the tools
    // are the Research agent's, and the prompt creates the initial queue.
    let sessions = provider.sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].label, "research");
    assert!(
        sessions[0]
            .system_prompt
            .as_deref()
            .is_some_and(|p| p.contains("You are the Research agent"))
    );
    assert_eq!(sessions[0].allowed_tools, RESEARCH_TOOLS);
    assert!(sessions[0].prompt.contains("initial task queue"));
    assert!(sessions[0].prompt.contains(TASK_FILE));
    // The report is saved under the bootstrap stem and covered by the reuse
    // window (T68.1, T69.1).
    assert_eq!(
        research_report_file(&fixture, "bootstrap"),
        "1. What does the project build?\nIt builds the greeting file."
    );
}

#[tokio::test]
async fn a_scan_queue_creation_run_is_a_research_session_whose_tasks_validate() {
    let tasks = "- [x] T1.1: first done\n";
    let fixture = Fixture::new(tasks);
    let provider = queue_research_writing(
        &format!("{tasks}- [ ] T2.1: fix the gap\n"),
        "1. What gap remains?\nThe greeting file is never verified.",
    );
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_queue_creation(QueueCreation::Scan).unwrap();
    let events = collect_until(&mut attachment.events, planning_finished).await;

    assert_research_run_announced(&events);
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Info && text == "1 task added.")
    );
    let Some(EngineEvent::TasksChanged { tasks }) = events
        .iter()
        .rev()
        .find(|e| matches!(e, EngineEvent::TasksChanged { .. }))
    else {
        panic!("no TasksChanged in {events:#?}");
    };
    let ids: Vec<_> = tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["T1.1", "T2.1"]);
    assert_eq!(engine.ui_added_task_ids(), ["T2.1"]);

    let sessions = provider.sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].label, "research");
    assert!(
        sessions[0]
            .system_prompt
            .as_deref()
            .is_some_and(|p| p.contains("You are the Research agent"))
    );
    assert_eq!(sessions[0].allowed_tools, RESEARCH_TOOLS);
    assert!(
        sessions[0]
            .prompt
            .contains("gaps and worthwhile follow-up work")
    );
    assert_eq!(
        research_report_file(&fixture, "scan"),
        "1. What gap remains?\nThe greeting file is never verified."
    );
}

#[tokio::test]
async fn a_fresh_queue_creation_report_is_reused_without_a_session() {
    let fixture = Fixture::new(TASKS);
    // A bootstrap report written moments ago: inside the ten-minute window.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let dir = fixture.data.path().join("research");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("bootstrap-{now}.md")),
        "1. Reused report.\n",
    )
    .unwrap();
    let provider = queue_research_writing("- [ ] T9.1: never appended\n", "1. Never run.");
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine
        .start_queue_creation(QueueCreation::Bootstrap)
        .unwrap();
    let events = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::Notice { text, .. } if text.contains("Reusing the research report for bootstrap"))
    })
    .await;

    // No session ran and the queue is untouched.
    assert!(provider.sessions().is_empty());
    assert!(!engine.is_planning());
    assert_eq!(fixture.tasks_file(), TASKS);
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Info
                && text == "Reusing the research report for bootstrap.")
    );
    // The reused report was not overwritten.
    assert_eq!(
        std::fs::read_to_string(dir.join(format!("bootstrap-{now}.md"))).unwrap(),
        "1. Reused report.\n"
    );
}

#[tokio::test]
async fn a_stale_queue_creation_report_runs_the_research_session() {
    let fixture = Fixture::new(TASKS);
    // A scan report twenty minutes old: outside the ten-minute window.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let dir = fixture.data.path().join("research");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("scan-{}.md", now - 1200)),
        "1. Stale report.\n",
    )
    .unwrap();
    let provider = queue_research_writing(
        &format!("{TASKS}- [ ] T2.1: fresh scan\n"),
        "1. Fresh report.",
    );
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_queue_creation(QueueCreation::Scan).unwrap();
    let events = collect_until(&mut attachment.events, planning_finished).await;

    // The research session ran; the fresh report replaced the stale one's role.
    assert_research_run_announced(&events);
    let sessions = provider.sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].label, "research");
    assert_eq!(research_report_file(&fixture, "scan"), "1. Fresh report.");
}

#[tokio::test]
async fn an_invalid_queue_creation_result_is_rolled_back_with_an_error() {
    let invalid = [
        // Modifies an existing task.
        TASKS.replace("- [ ] T1.1: add", "- [x] T1.1: add"),
        // Appends a task with a D-prefixed ID.
        format!("{TASKS}- [ ] D2.1: discovery\n"),
    ];
    for after in invalid {
        let fixture = Fixture::new(TASKS);
        let engine = fixture.engine(queue_research_writing(&after, "1. Report."));
        let mut attachment = engine.attach().unwrap();

        engine
            .start_queue_creation(QueueCreation::Bootstrap)
            .unwrap();
        let events = collect_until(&mut attachment.events, planning_finished).await;

        let notices = notices(&events);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].0, NoticeLevel::Error);
        assert!(
            notices[0].1.contains("research result rejected") && notices[0].1.contains("restored"),
            "{}",
            notices[0].1
        );
        assert_eq!(fixture.tasks_file(), TASKS);
        // A rejected result records no UI-added IDs and saves no report.
        assert!(engine.ui_added_task_ids().is_empty());
        assert!(!fixture.data.path().join("research").exists());
    }
}

#[tokio::test]
async fn a_discovery_round_appends_valid_d_tasks_and_the_queue_reconciles() {
    let fixture = Fixture::new(TASKS);
    let after = format!("{TASKS}\n- [ ] D2.1: first follow-up\n- [ ] D2.2: second follow-up\n");
    let provider = discovery_writing(&after);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_discovery().unwrap();
    let events = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::TasksChanged { .. })
    })
    .await;

    assert_eq!(
        notices(&events),
        [(NoticeLevel::Info, "2 tasks added.".to_string())]
    );
    let Some(EngineEvent::TasksChanged { tasks }) = events
        .iter()
        .rev()
        .find(|e| matches!(e, EngineEvent::TasksChanged { .. }))
    else {
        panic!("no TasksChanged in {events:#?}");
    };
    let ids: Vec<_> = tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["T1.1", "T1.2", "D2.1", "D2.2"]);
    assert!(tasks.iter().all(|t| !t.done));
    assert_eq!(fixture.tasks_file(), after);
    // Discovery-added tasks are not UI-added task IDs.
    assert!(engine.ui_added_task_ids().is_empty());

    let sessions = provider.sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].label, "discovery");
    assert!(sessions[0].prompt.contains("highest D number"));
    assert!(
        sessions[0]
            .system_prompt
            .as_deref()
            .is_some_and(|s| s.contains("Discovery agent"))
    );
    // The read-only Discovery allowlist.
    assert_eq!(
        sessions[0].allowed_tools,
        vec!["Read".to_string(), "Glob".to_string(), "Grep".to_string()]
    );
    // The raw stream is written to the history log like a planner session.
    assert_eq!(
        fixture
            .data
            .path()
            .join("history")
            .read_dir()
            .unwrap()
            .count(),
        1
    );

    // The round keeps the engine busy until it ends.
    tokio::time::timeout(Duration::from_secs(5), engine.wait_until_idle())
        .await
        .unwrap();
}

#[tokio::test]
async fn an_invalid_discovery_result_is_rolled_back_with_an_error() {
    let invalid = [
        // Modifies an existing task.
        TASKS.replace("- [ ] T1.1: add", "- [x] T1.1: add"),
        // Appends a task with a malformed ID.
        format!("{TASKS}- [ ] no id here\n"),
        // Appends non-D tasks: a T task, an H task and a bare-number ID.
        format!("{TASKS}- [ ] T2.1: planner style\n"),
        format!("{TASKS}- [ ] H2.1: human\n"),
        format!("{TASKS}- [ ] 2.1: bare number\n"),
    ];
    for after in invalid {
        let fixture = Fixture::new(TASKS);
        let engine = fixture.engine(discovery_writing(&after));
        let mut attachment = engine.attach().unwrap();

        engine.start_discovery().unwrap();
        let events = collect_until(&mut attachment.events, |e| {
            matches!(
                e,
                EngineEvent::Notice {
                    level: NoticeLevel::Error,
                    ..
                }
            )
        })
        .await;

        let notices = notices(&events);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(notices[0].0, NoticeLevel::Error);
        assert!(
            notices[0].1.contains("discovery result rejected") && notices[0].1.contains("restored"),
            "{}",
            notices[0].1
        );
        assert_eq!(fixture.tasks_file(), TASKS);
        assert!(engine.ui_added_task_ids().is_empty());
    }
}

#[tokio::test]
async fn a_discovery_round_that_adds_nothing_reports_no_tasks_added() {
    let tasks = "# Tasks\n- [x] T1.1: already done\n";
    let fixture = Fixture::new(tasks);
    let provider = MockProvider::new(vec![Step::Event(AgentEvent::Result {
        text: "nothing worth doing".into(),
    })]);
    let engine = fixture.engine(provider);
    let mut attachment = engine.attach().unwrap();

    engine.start_discovery().unwrap();
    let events = collect_until(&mut attachment.events, |e| {
        matches!(
            e,
            EngineEvent::Notice {
                level: NoticeLevel::Info,
                ..
            }
        )
    })
    .await;

    assert_eq!(
        notices(&events),
        [(NoticeLevel::Info, "No tasks added.".to_string())]
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EngineEvent::TasksChanged { .. }))
    );
    assert_eq!(fixture.tasks_file(), tasks);

    // The round is over and the engine is idle again: nothing pending, no run active.
    tokio::time::timeout(Duration::from_secs(5), engine.wait_until_idle())
        .await
        .unwrap();
    drop(attachment);
    assert!(engine.is_idle());
}

#[tokio::test]
async fn a_discovery_round_blocks_builds_and_planning_until_it_ends() {
    let fixture = Fixture::new(TASKS);
    let engine = fixture.engine(MockProvider::new(vec![Step::Sleep(Duration::from_millis(
        400,
    ))]));
    let mut attachment = engine.attach().unwrap();

    engine.start_discovery().unwrap();
    assert_eq!(
        engine.start_build().unwrap_err(),
        "a discovery round is running; wait for it to finish"
    );
    assert_eq!(
        engine.start_add_tasks("more").unwrap_err(),
        "a discovery round is running; wait for it to finish"
    );

    // The round announces itself to attached shells and reports its end the same way.
    let events = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::DiscoveryChanged { discovering: false })
    })
    .await;
    assert!(matches!(
        events.first(),
        Some(EngineEvent::DiscoveryChanged { discovering: true })
    ));
    // Once the round is over, builds are accepted again; the doubled cooldown (the round
    // added nothing) keeps the follow-up session free of another round.
    engine.start_build().unwrap();
    collect_until(&mut attachment.events, is_phase_startup).await;
}

#[tokio::test]
async fn completing_a_ui_added_task_postpones_the_discovery_round() {
    let tasks = "# Tasks\n- [x] T1.1: already done\n";
    let fixture = Fixture::new(tasks);
    let after = format!("{tasks}- [ ] T2.1: set from the ui\n");
    let provider = planner_writing(&after);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_add_tasks("one more").unwrap();
    collect_until(&mut attachment.events, planning_finished).await;

    // The UI-added task completes and empties the queue: the default cooldown (5 minutes)
    // postpones the round, so the session ends without one.
    engine.start_build().unwrap();
    collect_until(&mut attachment.events, is_phase_startup).await;

    let labels: Vec<_> = provider
        .sessions()
        .iter()
        .map(|s| s.label.clone())
        .collect();
    assert_eq!(labels, ["planner", "T2.1"]);
    assert!(
        fixture
            .tasks_file()
            .contains("- [x] T2.1: [--.B-] set from the ui")
    );
}

#[tokio::test]
async fn a_discovery_round_that_appends_tasks_continues_the_session() {
    let tasks = "# Tasks\n- [ ] T1.1: fix the only task\n";
    let fixture = Fixture::new(tasks);
    let discovered =
        "# Tasks\n- [x] T1.1: [--.B-] fix the only task\n- [ ] D1.1: fix the follow-up\n";
    let provider = MockProvider::per_session(vec![
        vec![Step::WriteFile {
            path: PathBuf::from("greeting.txt"),
            contents: "hello\n".into(),
        }],
        // The scheduled round on the emptied queue appends one D task.
        vec![Step::WriteFile {
            path: PathBuf::from("TASKS.md"),
            contents: discovered.into(),
        }],
        vec![Step::WriteFile {
            path: PathBuf::from("follow-up.txt"),
            contents: "done\n".into(),
        }],
    ]);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // T1.1, then the discovery round, then the discovered task — all in one session.
    let labels: Vec<_> = provider
        .sessions()
        .iter()
        .map(|s| s.label.clone())
        .collect();
    assert_eq!(labels, ["T1.1", "discovery", "D1.1"]);
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Info && text == "1 task added.")
    );
    assert!(
        fixture
            .tasks_file()
            .contains("- [x] D1.1: [--.B-] fix the follow-up")
    );
    assert!(
        fixture
            .git(&["log", "--format=%s"])
            .contains("feat(D1.1): fix the follow-up")
    );
    // The round that found work postponed the next one, so the session ended there.
    assert_eq!(provider.sessions().len(), 3);
}

/// The command behind the Enter key (T17.1): one round on an idle engine, appending
/// a D task through the same path an automatic round takes.
#[tokio::test]
async fn the_run_discovery_command_starts_a_round_which_appends_a_d_task() {
    let fixture = Fixture::new(TASKS);
    let after = format!("{TASKS}\n- [ ] D1.1: fix the greeting\n");
    let provider = discovery_writing(&after);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_discovery().unwrap();
    let events = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::DiscoveryChanged { discovering: false })
    })
    .await;

    // The round announced itself first and reported its end last.
    assert!(matches!(
        events.first(),
        Some(EngineEvent::DiscoveryChanged { discovering: true })
    ));
    assert_eq!(
        notices(&events)
            .iter()
            .find(|(level, _)| *level == NoticeLevel::Info)
            .map(|(_, text)| text.as_str()),
        Some("1 task added.")
    );
    // The queue reconciled with the appended D task.
    let Some(EngineEvent::TasksChanged { tasks }) = events
        .iter()
        .rev()
        .find(|e| matches!(e, EngineEvent::TasksChanged { .. }))
    else {
        panic!("no TasksChanged in {events:#?}");
    };
    let ids: Vec<_> = tasks.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["T1.1", "T1.2", "D1.1"]);
    assert_eq!(fixture.tasks_file(), after);
    assert!(!engine.is_discovering(), "the round is over");
    let sessions = provider.sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].label, "discovery");
}

/// The command is refused while a build, a planner run or another discovery round
/// is active (T17.1).
#[tokio::test]
async fn a_busy_engine_rejects_the_run_discovery_command() {
    let fixture = Fixture::new(TASKS);
    let engine = fixture.engine(MockProvider::new(vec![Step::Sleep(Duration::from_millis(
        400,
    ))]));
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    assert_eq!(
        engine.start_discovery().unwrap_err(),
        "a build is running; wait for it to finish"
    );
    collect_until(&mut attachment.events, is_phase_startup).await;

    let planner = fixture.engine(MockProvider::new(vec![Step::Sleep(Duration::from_millis(
        400,
    ))]));
    let mut attachment = planner.attach().unwrap();
    planner.start_add_tasks("first").unwrap();
    assert_eq!(
        planner.start_discovery().unwrap_err(),
        "tasks are being planned; wait for the planner to finish"
    );
    collect_until(&mut attachment.events, planning_finished).await;

    let discovery = fixture.engine(MockProvider::new(vec![Step::Sleep(Duration::from_millis(
        400,
    ))]));
    let mut attachment = discovery.attach().unwrap();
    discovery.start_discovery().unwrap();
    assert_eq!(
        discovery.start_discovery().unwrap_err(),
        "a discovery round is running; wait for it to finish"
    );
    collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::DiscoveryChanged { discovering: false })
    })
    .await;
}

/// The rail states from the events, in broadcast order.
fn rail(events: &[EngineEvent]) -> Vec<PipelineState> {
    events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::PipelineChanged { state } => Some(state.clone()),
            _ => None,
        })
        .collect()
}

/// The (plan, build, review, ship) tile statuses of one rail state.
fn walk(state: &PipelineState) -> (TileStatus, TileStatus, TileStatus, TileStatus) {
    (
        state.stage_status(Stage::Plan).unwrap(),
        state.stage_status(Stage::Build).unwrap(),
        state.stage_status(Stage::Review).unwrap(),
        state.ship,
    )
}

#[tokio::test]
async fn a_build_run_walks_the_pipeline_tiles_plan_build_ship() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        vec![Step::Event(AgentEvent::Result {
            text: VALID_PLAN.into(),
        })],
        builder_steps(),
    ]);
    // Sprint mode (T103.1), so the walk ends at the emptied queue without the
    // DISCOVER flips a scheduled round would add; the plan stage stays on.
    let mut config = fixture.config_sprint();
    config.plan_enabled = true;
    let engine = patok_engine::Engine::new(config, std::sync::Arc::new(provider));
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    let rail = rail(&events);
    // The stages the engine runs today, with their letters.
    assert_eq!(
        rail[0]
            .stages
            .iter()
            .map(|tile| tile.stage.letters())
            .collect::<Vec<_>>(),
        ["R", "P", "B", "RV"]
    );
    // Plan pending, then active in the plan session, then done while the builder
    // runs; the build tile goes done when the builder session completes, so the
    // stages before the running one are always done. The review stage is skipped
    // (review off), so its tile goes done with the skip; then ship active around
    // the commit, then everything muted again.
    let walked: Vec<_> = rail.iter().map(walk).collect();
    assert_eq!(
        walked,
        [
            (
                TileStatus::Pending,
                TileStatus::Pending,
                TileStatus::Pending,
                TileStatus::Muted
            ),
            (
                TileStatus::Active,
                TileStatus::Pending,
                TileStatus::Pending,
                TileStatus::Muted
            ),
            (
                TileStatus::Done,
                TileStatus::Active,
                TileStatus::Pending,
                TileStatus::Muted
            ),
            (
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Pending,
                TileStatus::Muted
            ),
            (
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Muted
            ),
            (
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Active
            ),
            (
                TileStatus::Muted,
                TileStatus::Muted,
                TileStatus::Muted,
                TileStatus::Muted
            ),
        ],
        "{rail:#?}"
    );
}

#[tokio::test]
async fn a_build_without_the_plan_stage_walks_build_directly() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    // Sprint mode (T103.1), so the walk ends at the emptied queue without the
    // DISCOVER flips a scheduled round would add.
    let engine = fixture.engine_sprint(scripted());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The plan stage is disabled: the plan tile is still done once the builder
    // runs, so the stages before the running one are never stale. The build tile
    // goes done when the builder session completes. The review stage is skipped
    // (review off), so its tile goes done with the skip.
    let walked: Vec<_> = rail(&events).iter().map(walk).collect();
    assert_eq!(
        walked,
        [
            (
                TileStatus::Pending,
                TileStatus::Pending,
                TileStatus::Pending,
                TileStatus::Muted
            ),
            (
                TileStatus::Done,
                TileStatus::Active,
                TileStatus::Pending,
                TileStatus::Muted
            ),
            (
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Pending,
                TileStatus::Muted
            ),
            (
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Muted
            ),
            (
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Active
            ),
            (
                TileStatus::Muted,
                TileStatus::Muted,
                TileStatus::Muted,
                TileStatus::Muted
            ),
        ],
        "{:#?}",
        rail(&events)
    );
}

#[tokio::test]
async fn a_review_run_shows_the_build_tile_done_while_the_reviewer_is_active() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        builder_with_claims("greeting.txt", "hello\n", "none"),
        review_pass(),
    ]);
    // The review must run: the simple-skip rule is off and the thresholds'
    // defaults leave it eligible.
    let engine = fixture.engine_configured(provider, "skip_review_for_simple = false\n");
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    let walked: Vec<_> = rail(&events).iter().map(walk).collect();
    // The build tile is done while the reviewer runs: the stages before the
    // running one are always done.
    assert!(
        walked.contains(&(
            TileStatus::Done,
            TileStatus::Done,
            TileStatus::Active,
            TileStatus::Muted
        )),
        "the reviewer-active rail state is missing: {walked:#?}"
    );
    // The regression guard: the build and review tiles are never both
    // in-progress at once.
    assert!(
        !walked
            .iter()
            .any(|(_, build, review, _)| *build == TileStatus::Active
                && *review == TileStatus::Active),
        "the build tile is still in-progress while the reviewer runs: {walked:#?}"
    );
    assert_eq!(
        walked.last().unwrap(),
        &(
            TileStatus::Muted,
            TileStatus::Muted,
            TileStatus::Muted,
            TileStatus::Muted
        ),
        "{walked:#?}"
    );
}

#[tokio::test]
async fn a_discovery_round_flips_discover_from_active_to_done() {
    let fixture = Fixture::new(TASKS);
    // A completed round that appends nothing.
    let engine = fixture.engine(MockProvider::new(vec![]));
    let mut attachment = engine.attach().unwrap();
    engine
        .apply_settings_change(
            "run_mode",
            patok_core::config::SettingValue::Str("continuous".into()),
        )
        .unwrap();

    engine.start_discovery().unwrap();
    let events = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::DiscoveryChanged { discovering: false })
    })
    .await;

    let rail = rail(&events);
    assert!(
        rail.iter()
            .any(|state| state.discover == TileStatus::Active),
        "the round's own rail state is missing: {rail:#?}"
    );
    assert_eq!(rail.last().unwrap().discover, TileStatus::Done);
}

#[tokio::test]
async fn sprint_run_mode_keeps_discover_muted() {
    let fixture = Fixture::new(TASKS);
    // The sprint engine (T103.1): the manual round stays available, but the
    // DISCOVER tile never unmutes.
    let engine = fixture.engine_sprint(MockProvider::new(vec![]));
    let mut attachment = engine.attach().unwrap();

    engine.start_discovery().unwrap();
    let events = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::DiscoveryChanged { discovering: false })
    })
    .await;

    assert!(
        rail(&events)
            .iter()
            .all(|state| state.discover == TileStatus::Muted)
    );
    assert_eq!(attachment.snapshot.pipeline.discover, TileStatus::Muted);
}

#[tokio::test]
async fn an_idle_engine_reports_every_tile_muted() {
    let fixture = Fixture::new(TASKS);
    let engine = fixture.engine(scripted());
    let attachment = engine.attach().unwrap();

    let pipeline = &attachment.snapshot.pipeline;
    assert_eq!(
        pipeline.stages,
        [
            Tile {
                stage: Stage::Research,
                status: TileStatus::Muted
            },
            Tile {
                stage: Stage::Plan,
                status: TileStatus::Muted
            },
            Tile {
                stage: Stage::Build,
                status: TileStatus::Muted
            },
            Tile {
                stage: Stage::Review,
                status: TileStatus::Muted
            }
        ]
    );
    assert_eq!(pipeline.ship, TileStatus::Muted);
    assert_eq!(pipeline.discover, TileStatus::Muted);
    assert_eq!(pipeline.learnings, TileStatus::Muted);
}

#[tokio::test]
async fn an_attaching_shell_receives_the_current_pipeline_state() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        vec![
            Step::Sleep(Duration::from_secs(2)),
            Step::Event(AgentEvent::Result {
                text: VALID_PLAN.into(),
            }),
        ],
        builder_steps(),
    ]);
    let engine = fixture.engine_with_plan(provider);
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    collect_until(&mut attachment.events, |e| {
        matches!(
            e,
            EngineEvent::PipelineChanged { state }
                if state.stage_status(Stage::Plan) == Some(TileStatus::Active)
        )
    })
    .await;
    drop(attachment);

    // The reattaching shell sees the mid-run rail in the snapshot and a replayed
    // pipeline state among the recent events.
    let mut attachment = engine.attach().unwrap();
    let pipeline = &attachment.snapshot.pipeline;
    assert_eq!(pipeline.stage_status(Stage::Plan), Some(TileStatus::Active));
    assert_eq!(
        pipeline.stage_status(Stage::Build),
        Some(TileStatus::Pending)
    );
    assert!(
        attachment
            .snapshot
            .recent
            .iter()
            .any(|e| matches!(e, EngineEvent::PipelineChanged { .. }))
    );
    collect_until(&mut attachment.events, is_phase_startup).await;
}

#[tokio::test]
async fn a_learned_learning_marks_the_learnings_tile_done() {
    let fixture = Fixture::new(TASKS);
    let engine = fixture.engine(scripted());
    let mut attachment = engine.attach().unwrap();

    engine.record_learning_learned();
    let events = collect_until(&mut attachment.events, |e| {
        matches!(e, EngineEvent::PipelineChanged { .. })
    })
    .await;

    assert_eq!(
        rail(&events)[0],
        PipelineState {
            learnings: TileStatus::Done,
            ..PipelineState::today()
        }
    );
}

mod provider_config {
    use std::sync::{Arc, Mutex};

    use patok_core::config::ProviderKind;
    use patok_providers::{Provider, ProviderError};

    use super::*;

    fn write(path: &std::path::Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// An engine configured from `user` and `project` config text; returns the kinds the
    /// resolver was asked for.
    fn engine_with(
        user: Option<&str>,
        project: Option<&str>,
        resolve: impl Fn(ProviderKind) -> Result<Arc<dyn Provider>, ProviderError>
        + Send
        + Sync
        + 'static,
    ) -> (Fixture, patok_engine::Engine) {
        let fixture = Fixture::new(TASKS);
        let user_file = fixture.data.path().join("user/patok/config.toml");
        if let Some(text) = user {
            write(&user_file, text);
        }
        if let Some(text) = project {
            write(&fixture.project_config(), text);
        }
        let mut config = fixture.config();
        config.config_files.user_global = Some(user_file);
        let engine = patok_engine::Engine::configured(config, resolve);
        (fixture, engine)
    }

    fn picking(
        picked: &Arc<Mutex<Vec<ProviderKind>>>,
    ) -> impl Fn(ProviderKind) -> Result<Arc<dyn Provider>, ProviderError> + Send + Sync + 'static
    {
        let picked = picked.clone();
        move |kind| {
            picked.lock().unwrap().push(kind);
            Ok(Arc::new(scripted()))
        }
    }

    #[tokio::test]
    async fn defaults_to_claude_for_build_and_planner() {
        let picked = Arc::default();
        let (_fixture, engine) = engine_with(None, None, picking(&picked));
        assert_eq!(*picked.lock().unwrap(), [ProviderKind::Claude]);
        let mut attachment = engine.attach().unwrap();
        assert_eq!(attachment.snapshot.provider, "claude");
        assert_eq!(attachment.snapshot.model, "");
        engine.start_build().unwrap();
        collect_until(&mut attachment.events, is_phase_startup).await;
    }

    #[tokio::test]
    async fn mistral_is_selected_from_config() {
        let picked = Arc::default();
        let (_fixture, engine) = engine_with(
            None,
            Some("[daemon]\nprovider = \"mistral\"\nmodel = \"mistral-small\"\n"),
            picking(&picked),
        );
        assert_eq!(*picked.lock().unwrap(), [ProviderKind::Mistral]);
        let snapshot = engine.attach().unwrap().snapshot.clone();
        assert_eq!(snapshot.provider, "mistral");
        assert_eq!(snapshot.model, "mistral-small");
    }

    #[tokio::test]
    async fn project_layer_overrides_user_layer() {
        let picked = Arc::default();
        let (_f, engine) = engine_with(
            Some("[daemon]\nprovider = \"mistral\"\n"),
            Some("[daemon]\nprovider = \"claude\"\n"),
            picking(&picked),
        );
        assert_eq!(*picked.lock().unwrap(), [ProviderKind::Claude]);
        drop(engine);

        // The user layer applies where the project sets nothing.
        let picked = Arc::default();
        let (_f, _engine) = engine_with(
            Some("[daemon]\nprovider = \"mistral\"\n"),
            Some("[daemon]\nmodel = \"m\"\n"),
            picking(&picked),
        );
        assert_eq!(*picked.lock().unwrap(), [ProviderKind::Mistral]);
    }

    #[tokio::test]
    async fn missing_cli_is_a_command_error_not_a_crash() {
        let (_fixture, engine) = engine_with(None, None, |_| {
            Err(ProviderError::CliNotFound {
                cli: "claude".into(),
            })
        });
        let attachment = engine.attach().unwrap();
        assert_eq!(attachment.snapshot.phase, Phase::Startup);
        let build = engine.start_build().unwrap_err();
        assert!(build.contains("`claude` CLI was not found"), "{build}");
        let plan = engine.start_add_tasks("something").unwrap_err();
        assert!(plan.contains("`claude` CLI was not found"), "{plan}");
    }

    #[tokio::test]
    async fn codex_provider_without_the_cli_is_a_command_error_not_a_crash() {
        let (_fixture, engine) =
            engine_with(None, Some("[daemon]\nprovider = \"codex\"\n"), |_| {
                Err(ProviderError::CliNotFound {
                    cli: "codex".into(),
                })
            });
        let attachment = engine.attach().unwrap();
        assert_eq!(attachment.snapshot.phase, Phase::Startup);
        let build = engine.start_build().unwrap_err();
        assert!(build.contains("`codex` CLI was not found"), "{build}");
        let plan = engine.start_add_tasks("something").unwrap_err();
        assert!(plan.contains("`codex` CLI was not found"), "{plan}");
    }

    #[tokio::test]
    async fn invalid_config_is_a_command_error_naming_the_file() {
        let (_fixture, engine) = engine_with(None, Some("[daemon]\nprovider = \"gpt\"\n"), |_| {
            unreachable!("an invalid config must not resolve a provider")
        });
        let error = engine.start_build().unwrap_err();
        assert!(error.contains("patok.config.toml"), "{error}");
        assert!(error.contains("provider"), "{error}");
    }

    #[tokio::test]
    async fn an_invalid_plan_enabled_falls_back_to_the_default() {
        // A wrong-typed field falls back to its default with a warning naming the file
        // and the field; the build is not refused.
        let (_fixture, engine) = engine_with(
            None,
            Some("[daemon]\nplan_enabled = \"yes\"\n"),
            picking(&Arc::default()),
        );
        engine.start_build().unwrap();
    }

    #[tokio::test]
    async fn the_plan_stage_runs_by_default_from_the_config() {
        let fixture = Fixture::new("- [ ] T1.1: fix the only task\n");
        let provider = MockProvider::per_session(vec![
            vec![Step::Event(AgentEvent::Result {
                text: VALID_PLAN.into(),
            })],
            builder_steps(),
        ]);
        let recorded = provider.clone();
        let provider = Arc::new(provider);
        let engine =
            patok_engine::Engine::configured(fixture.config(), move |_| Ok(provider.clone()));
        let mut attachment = engine.attach().unwrap();

        engine.start_build().unwrap();
        collect_until(&mut attachment.events, is_phase_startup).await;

        let labels: Vec<_> = recorded
            .sessions()
            .iter()
            .map(|s| s.label.clone())
            .collect();
        // No trailing discovery round: the config files default to sprint
        // mode (T103.1), which ends the session on the emptied queue.
        assert_eq!(labels, ["plan", "T1.1"]);
    }

    #[tokio::test]
    async fn plan_enabled_false_from_the_config_skips_the_plan_stage() {
        let fixture = Fixture::new("- [ ] T1.1: fix the only task\n");
        write(
            &fixture.project_config(),
            "[daemon]\nplan_enabled = false\n",
        );
        let provider = MockProvider::new(builder_steps());
        let recorded = provider.clone();
        let provider = Arc::new(provider);
        let engine =
            patok_engine::Engine::configured(fixture.config(), move |_| Ok(provider.clone()));
        let mut attachment = engine.attach().unwrap();

        engine.start_build().unwrap();
        collect_until(&mut attachment.events, is_phase_startup).await;

        let labels: Vec<_> = recorded
            .sessions()
            .iter()
            .map(|s| s.label.clone())
            .collect();
        // No trailing discovery round: the config files default to sprint
        // mode (T103.1), which ends the session on the emptied queue.
        assert_eq!(labels, ["T1.1"]);
    }

    #[tokio::test]
    async fn the_configured_discovery_cooldown_reaches_the_engine() {
        let tasks = "# Tasks\n- [x] T1.1: already done\n";
        let fixture = Fixture::new(tasks);
        // A 1-second cooldown, so the test can wait it out; the plan stage is off so the
        // build sessions stay plain builder sessions, and the run mode is continuous so
        // the scheduled round runs at all (T103.1).
        write(
            &fixture.project_config(),
            "[daemon]\ndiscovery_cooldown_secs = 1\nplan_enabled = false\nrun_mode = \"continuous\"\n",
        );
        let appended = format!("{tasks}- [ ] T2.1: set from the ui\n");
        let provider = MockProvider::per_session(vec![
            // The planner session appends the UI-added task.
            vec![Step::WriteFile {
                path: PathBuf::from("TASKS.md"),
                contents: appended,
            }],
            // The two build sessions change nothing.
            vec![],
            vec![],
            // The discovery round finds nothing.
            vec![Step::Event(AgentEvent::Result {
                text: "nothing worth doing".into(),
            })],
        ]);
        let recorded = provider.clone();
        let provider = Arc::new(provider);
        let engine =
            patok_engine::Engine::configured(fixture.config(), move |_| Ok(provider.clone()));
        let mut attachment = engine.attach().unwrap();

        engine.start_add_tasks("one more").unwrap();
        collect_until(&mut attachment.events, planning_finished).await;
        engine.start_build().unwrap();
        collect_until(&mut attachment.events, is_phase_startup).await;

        // Completing the UI-added task postponed the round by the configured cooldown,
        // so the emptied queue ended the session without one.
        let labels: Vec<_> = recorded
            .sessions()
            .iter()
            .map(|s| s.label.clone())
            .collect();
        assert_eq!(labels, ["planner", "T2.1"]);

        // Once the cooldown has elapsed, a later session runs its discovery round.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        std::fs::write(
            fixture.path().join("TASKS.md"),
            format!("{tasks}- [x] T2.1: set from the ui\n- [ ] T2.3: set externally\n"),
        )
        .unwrap();
        engine.start_build().unwrap();
        collect_until(&mut attachment.events, is_phase_startup).await;

        let labels: Vec<_> = recorded
            .sessions()
            .iter()
            .map(|s| s.label.clone())
            .collect();
        assert_eq!(labels, ["planner", "T2.1", "T2.3", "discovery"]);
    }
}

// ---- The review stage (T70.1) ----

/// A builder script whose final message ends with the build claims section
///.
fn builder_with_claims(path: &str, contents: &str, claims_extra: &str) -> Vec<Step> {
    vec![
        Step::WriteFile {
            path: PathBuf::from(path),
            contents: contents.into(),
        },
        Step::Event(AgentEvent::Result {
            text: format!(
                "Done.\n\n## Build Claims\n## Files Changed\nCREATE {path}\n\n## Verification Results\ncat {path} -- PASS\n\n## Claims\n- [ ] the file exists\n\n## Wire-Up Evidence\nN/A: no new public surface\n\n## Gaps and Assumptions\n{claims_extra}\n"
            ),
        }),
    ]
}

/// A reviewer script whose final message ends with a passing review report.
fn review_pass() -> Vec<Step> {
    vec![Step::Event(AgentEvent::Result {
        text: "The claims check out.\n\nVerdict: PASS\n\n{\"high\": [], \"medium\": [], \"low\": []}\n"
            .into(),
    })]
}

/// A reviewer script whose final message ends with a failing review report
/// carrying one qualifying HIGH finding.
fn review_fail() -> Vec<Step> {
    vec![Step::Event(AgentEvent::Result {
        text: "Verdict: FAIL\n\n{\"high\": [{\"file\": \"greeting.txt\", \"line\": 1, \"issue\": \"the greeting is wrong\", \"fixed\": false, \"category\": \"logic\", \"source\": \"the file\", \"confidence\": 0.9}], \"medium\": [], \"low\": []}\n".into(),
    })]
}

/// One review artifact file of `folder` written for `task_id`, with its contents.
fn artifact(fixture: &Fixture, folder: &str, task_id: &str) -> Vec<(String, String)> {
    std::fs::read_dir(fixture.data_path(folder))
        .unwrap()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(entry.path()).ok()?;
            name.starts_with(&format!("{task_id}-"))
                .then_some((name, text))
        })
        .collect()
}

#[tokio::test]
async fn a_passing_review_validates_the_task_and_commits_feat() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        builder_with_claims("greeting.txt", "hello\n", "none"),
        review_pass(),
    ]);
    // The review must run: the simple-skip rule is off and the thresholds'
    // defaults leave it eligible.
    let engine = fixture.engine_configured(provider.clone(), "skip_review_for_simple = false\n");
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Done,
            commit: Some(_),
            ..
        }
    )));
    // The claims and review artifacts (T70.1), saved under the project data
    // directory next to the plans and research folders.
    let claims = artifact(&fixture, "claims", "T1.1");
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert!(claims[0].0.ends_with(".md"));
    assert!(claims[0].1.contains("## Build Claims"));
    assert!(claims[0].1.contains("- [ ] the file exists"));
    let reviews = artifact(&fixture, "reviews", "T1.1");
    assert_eq!(reviews.len(), 1, "{reviews:?}");
    assert!(reviews[0].1.contains("Verdict: PASS"));

    // One builder session then one reviewer session with the reviewer
    // allowlist; the reviewer's raw stream lands in the history log.
    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    // The sprint config ends the session on the emptied queue (T103.1), so no
    // discovery round follows the reviewer.
    assert_eq!(labels, ["T1.1", "review"]);
    assert_eq!(
        sessions[1].allowed_tools,
        ["Read", "Glob", "Grep", "Bash", "Edit", "Write"]
    );
    // The reviewer prompt carries the task, the claims, the changed files
    // (the diff is empty for an untracked file) and the pass number.
    assert!(
        sessions[1]
            .prompt
            .contains("Task T1.1: add the greeting file")
    );
    assert!(sessions[1].prompt.contains("## Build Claims"));
    assert!(sessions[1].prompt.contains("- [ ] the file exists"));
    assert!(sessions[1].prompt.contains("- greeting.txt"));
    assert!(sessions[1].prompt.contains("Pass 1."));
    assert!(
        sessions[1]
            .system_prompt
            .as_deref()
            .is_some_and(|system| system.starts_with("You are the Reviewer agent"))
    );

    // The task is validated and ticked with the five-position indicator, and
    // committed as feat.
    assert!(
        fixture
            .tasks_file()
            .contains("- [x] T1.1: [--.BR] add the greeting file")
    );
    let log = fixture.git(&["log", "--format=%s"]);
    assert!(log.contains("feat(T1.1): add the greeting file"), "{log}");
    // The builder and reviewer raw streams are in the history log.
    assert_eq!(fixture.data_path("history").read_dir().unwrap().count(), 2);
}

#[tokio::test]
async fn reviewer_provider_codex_runs_the_reviewer_on_codex() {
    use patok_core::config::ProviderKind;

    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        builder_with_claims("greeting.txt", "hello\n", "none"),
        review_pass(),
    ]);
    let recorded = provider.clone();
    std::fs::write(
        fixture.project_config(),
        "[daemon]\nplan_enabled = false\nrun_mode = \"sprint\"\nreview_in_loop = true\nskip_review_for_simple = false\nreviewer_provider = \"codex\"\n",
    )
    .unwrap();
    let picked: std::sync::Arc<std::sync::Mutex<Vec<ProviderKind>>> = std::sync::Arc::default();
    let resolver_picked = picked.clone();
    let engine = patok_engine::Engine::configured_with_env(
        fixture.config_review(),
        &patok_core::config::DaemonEnv::default(),
        move |kind| {
            resolver_picked.lock().unwrap().push(kind);
            Ok(std::sync::Arc::new(provider.clone()))
        },
    );
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let _events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The reviewer session was routed onto the codex provider kind.
    assert!(
        picked.lock().unwrap().contains(&ProviderKind::Codex),
        "{:?}",
        picked.lock().unwrap()
    );
    let sessions = recorded.sessions();
    let labels: Vec<&str> = sessions.iter().map(|s| s.label.as_str()).collect();
    assert!(labels.contains(&"review"), "{labels:?}");
}

/// The claude reviewer provider routes the reviewer onto the claude provider with
/// the configured reviewer model, even while the main provider is another one
/// (T81.1): the resolver is asked for the claude kind, the starting event and
/// the routed session name claude and the reviewer model, and the builder
/// session keeps the main provider and its own model.
#[tokio::test]
async fn reviewer_provider_claude_runs_the_reviewer_on_the_claude_provider_with_the_reviewer_model()
{
    use patok_core::config::ProviderKind;

    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        builder_with_claims("greeting.txt", "hello\n", "none"),
        review_pass(),
    ]);
    let recorded = provider.clone();
    std::fs::write(
        fixture.project_config(),
        "[daemon]\nplan_enabled = false\nrun_mode = \"sprint\"\nreview_in_loop = true\nskip_review_for_simple = false\nprovider = \"mistral\"\nreviewer_provider = \"claude\"\nreviewer_model = \"opus\"\n",
    )
    .unwrap();
    let picked: std::sync::Arc<std::sync::Mutex<Vec<ProviderKind>>> = std::sync::Arc::default();
    let resolver_picked = picked.clone();
    let engine = patok_engine::Engine::configured_with_env(
        fixture.config_review(),
        &patok_core::config::DaemonEnv::default(),
        move |kind| {
            resolver_picked.lock().unwrap().push(kind);
            Ok(std::sync::Arc::new(
                provider.clone().with_slug(kind.as_str()),
            ))
        },
    );
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The reviewer session was routed onto the claude provider kind.
    assert!(
        picked.lock().unwrap().contains(&ProviderKind::Claude),
        "{:?}",
        picked.lock().unwrap()
    );
    // The starting events name each session's actual provider and model:
    // the reviewer runs on claude with the reviewer model, the builder on
    // the configured mistral with no model.
    assert!(
        started(&events).contains(&("reviewer".into(), "claude".into(), Some("opus".into()))),
        "{:#?}",
        started(&events)
    );
    assert!(
        started(&events).contains(&("builder".into(), "mistral".into(), None)),
        "{:#?}",
        started(&events)
    );
    // The recorded sessions show the same routing: the review session runs
    // with the reviewer model, the builder session without a model.
    let sessions = recorded.sessions();
    let review = sessions
        .iter()
        .find(|s| s.label == "review")
        .expect("the review session ran");
    assert_eq!(review.model.as_deref(), Some("opus"));
    let build = sessions
        .iter()
        .find(|s| s.label == "T1.1")
        .expect("the builder session ran");
    assert!(build.model.is_none());
}

/// The mistral reviewer provider (including its `vibe` alias spelling) routes
/// the reviewer onto the mistral provider with no model (the provider picks
/// its default).
#[tokio::test]
async fn reviewer_provider_mistral_runs_the_reviewer_on_the_mistral_provider() {
    use patok_core::config::ProviderKind;

    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        builder_with_claims("greeting.txt", "hello\n", "none"),
        review_pass(),
    ]);
    let recorded = provider.clone();
    std::fs::write(
        fixture.project_config(),
        "[daemon]\nplan_enabled = false\nrun_mode = \"sprint\"\nreview_in_loop = true\nskip_review_for_simple = false\nprovider = \"mistral\"\nreviewer_provider = \"mistral\"\nreviewer_model = \"opus\"\n",
    )
    .unwrap();
    let picked: std::sync::Arc<std::sync::Mutex<Vec<ProviderKind>>> = std::sync::Arc::default();
    let resolver_picked = picked.clone();
    let engine = patok_engine::Engine::configured_with_env(
        fixture.config_review(),
        &patok_core::config::DaemonEnv::default(),
        move |kind| {
            resolver_picked.lock().unwrap().push(kind);
            Ok(std::sync::Arc::new(
                provider.clone().with_slug(kind.as_str()),
            ))
        },
    );
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    assert!(
        picked.lock().unwrap().contains(&ProviderKind::Mistral),
        "{:?}",
        picked.lock().unwrap()
    );
    assert!(
        started(&events).contains(&("reviewer".into(), "mistral".into(), None)),
        "{:#?}",
        started(&events)
    );
    let sessions = recorded.sessions();
    let review = sessions
        .iter()
        .find(|s| s.label == "review")
        .expect("the review session ran");
    assert!(review.model.is_none());
}

/// A stored config carrying a daemon key the schema no longer knows keeps
/// running (T91.1): the key falls to the unknown-key warning, the config
/// loads, and the review runs on the default reviewer provider.
#[tokio::test]
async fn a_config_with_an_unknown_daemon_key_still_runs_the_review() {
    use patok_core::config::ProviderKind;

    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        builder_with_claims("greeting.txt", "hello\n", "none"),
        review_pass(),
    ]);
    let recorded = provider.clone();
    std::fs::write(
        fixture.project_config(),
        "[daemon]\nplan_enabled = false\nrun_mode = \"sprint\"\nreview_in_loop = true\nskip_review_for_simple = false\nlegacy_review_routing = \"codex\"\n",
    )
    .unwrap();
    let picked: std::sync::Arc<std::sync::Mutex<Vec<ProviderKind>>> = std::sync::Arc::default();
    let resolver_picked = picked.clone();
    let engine = patok_engine::Engine::configured_with_env(
        fixture.config_review(),
        &patok_core::config::DaemonEnv::default(),
        move |kind| {
            resolver_picked.lock().unwrap().push(kind);
            Ok(std::sync::Arc::new(
                provider.clone().with_slug(kind.as_str()),
            ))
        },
    );
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The unrecognized key changes nothing: the reviewer runs on the default
    // claude provider with the default reviewer model, not on codex.
    assert!(
        picked.lock().unwrap().contains(&ProviderKind::Claude),
        "{:?}",
        picked.lock().unwrap()
    );
    assert!(
        !picked.lock().unwrap().contains(&ProviderKind::Codex),
        "{:?}",
        picked.lock().unwrap()
    );
    assert!(
        started(&events).contains(&("reviewer".into(), "claude".into(), Some("sonnet".into()))),
        "{:#?}",
        started(&events)
    );
    let sessions = recorded.sessions();
    let review = sessions
        .iter()
        .find(|s| s.label == "review")
        .expect("the review session ran");
    assert_eq!(review.model.as_deref(), Some("sonnet"));
}

#[tokio::test]
async fn a_fail_verdict_produces_a_wip_commit_with_the_task_not_validated() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        builder_with_claims("greeting.txt", "hello\n", "none"),
        review_fail(),
    ]);
    let engine = fixture.engine_configured(provider.clone(), "skip_review_for_simple = false\n");
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The failed review is a stage failure whose suggestions name the report.
    assert!(notices(&events).iter().any(|(level, text)| {
        *level == NoticeLevel::Error
            && text
                .starts_with("review failed for T1.1: HIGH/MEDIUM issues remain, check report at ")
    }));
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Failed,
            ..
        }
    )));
    // The task stays unchecked, with the exclamation-marked indicator.
    assert!(
        fixture
            .tasks_file()
            .contains("- [ ] T1.1: [--.BR!] add the greeting file")
    );
    let log = fixture.git(&["log", "--format=%s"]);
    assert!(log.contains("WIP(T1.1): add the greeting file"), "{log}");
}

#[tokio::test]
async fn an_invalid_findings_block_fails_the_review() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        builder_with_claims("greeting.txt", "hello\n", "none"),
        // A verdict line but no valid findings JSON: the absent block counts
        // as one HIGH finding, and only a pass verdict could carry it.
        vec![Step::Event(AgentEvent::Result {
            text: "Verdict: FAIL\n\nThe findings block got mangled: {\"high\": \n".into(),
        })],
    ]);
    let engine = fixture.engine_configured(provider.clone(), "skip_review_for_simple = false\n");
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Failed,
            ..
        }
    )));
    assert!(
        fixture
            .tasks_file()
            .contains("- [ ] T1.1: [--.BR!] add the greeting file")
    );
    let log = fixture.git(&["log", "--format=%s"]);
    assert!(log.contains("WIP(T1.1)"), "{log}");
}

#[tokio::test]
async fn no_changed_files_fails_the_review() {
    // A medium description, so the simple-skip rule never fires.
    let fixture = Fixture::new("- [ ] T1.1: introduce the greeting file for the review stage\n");
    let provider = MockProvider::per_session(vec![
        // Research runs for a medium task; the builder changes nothing.
        research_steps("1. What exists? Answer: nothing."),
        vec![Step::Event(AgentEvent::Result {
            text: "nothing to do".into(),
        })],
    ]);
    let engine = fixture.engine_with_review(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    assert!(notices(&events).iter().any(|(level, text)| {
        *level == NoticeLevel::Error && text == "T1.1: no changed files to review"
    }));
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Failed,
            ..
        }
    )));
    // No reviewer session ever ran.
    let labels: Vec<String> = provider
        .sessions()
        .iter()
        .map(|s| s.label.clone())
        .collect();
    assert_eq!(labels, ["research", "T1.1"]);
    assert!(
        fixture
            .tasks_file()
            .contains("- [ ] T1.1: [R-.BR!] introduce the greeting file for the review stage")
    );
    let log = fixture.git(&["log", "--format=%s"]);
    assert!(log.contains("WIP(T1.1)"), "{log}");
}

#[tokio::test]
async fn review_in_loop_off_skips_the_review_with_its_reason() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::new(builder_steps());
    // The default fixture engine has the review stage off.
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    assert!(notices(&events).iter().any(|(level, text)| {
        *level == NoticeLevel::Info
            && text == "Review skipped for T1.1: review in loop is off; the task is validated by the builder's own verification."
    }));
    // The skip treats the task as validated: feat commit and the dash review
    // indicator.
    assert!(
        fixture
            .tasks_file()
            .contains("- [x] T1.1: [--.B-] add the greeting file")
    );
    let log = fixture.git(&["log", "--format=%s"]);
    assert!(log.contains("feat(T1.1)"), "{log}");
}

#[tokio::test]
async fn a_simple_task_with_a_passing_build_skips_the_review() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::new(builder_steps());
    let engine = fixture.engine_with_review(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    assert!(notices(&events).iter().any(|(level, text)| {
        *level == NoticeLevel::Info
            && text == "Review skipped for T1.1: a simple task whose builder verification exited cleanly; skip review for simple."
    }));
    // The key off means the review runs.
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![builder_steps(), review_pass()]);
    let engine = fixture.engine_configured(provider.clone(), "skip_review_for_simple = false\n");
    let mut attachment = engine.attach().unwrap();
    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Done,
            ..
        }
    )));
    // Builder and reviewer sessions; the sprint config ends the session on the
    // emptied queue (T103.1).
    assert_eq!(provider.sessions().len(), 2);
}

#[tokio::test]
async fn learned_confidence_skips_the_review_after_the_threshold() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    // Prime the persistent history with two consecutive review passes for the
    // task's shape.
    let primed_history = r#"{"clusters":[{"representative":"add the greeting file","tokens":["add","greeting","file"],"tier":"simple","passes":2,"fails":0,"consecutive_passes":2,"last_fail":null}]}"#;
    std::fs::write(fixture.data_path("review-history.json"), primed_history).unwrap();
    let provider = MockProvider::new(builder_steps());
    let engine = fixture.engine_configured(
        provider.clone(),
        "skip_review_for_simple = false\nreview_confidence_threshold = 2\n",
    );
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    assert!(notices(&events).iter().any(|(level, text)| {
        *level == NoticeLevel::Info
            && text == "Review skipped for T1.1: learned confidence: similar tasks passed review repeatedly."
    }));
    // Only the builder session: the sprint config ends the session on the
    // emptied queue (T103.1).
    assert_eq!(provider.sessions().len(), 1);
    // A threshold of 0 disables the feature: the review runs.
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    std::fs::write(fixture.data_path("review-history.json"), primed_history).unwrap();
    let provider = MockProvider::per_session(vec![builder_steps(), review_pass()]);
    let engine = fixture.engine_configured(
        provider.clone(),
        "skip_review_for_simple = false\nreview_confidence_threshold = 0\n",
    );
    let mut attachment = engine.attach().unwrap();
    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Done,
            ..
        }
    )));
    // Builder, reviewer; the sprint config ends the session on the emptied
    // queue (T103.1).
    assert_eq!(provider.sessions().len(), 2);
}

#[tokio::test]
async fn a_disabled_review_stage_skips_the_review() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::new(builder_steps());
    let engine = fixture.engine_configured(
        provider.clone(),
        "skip_review_for_simple = false\n\n[[daemon.stages]]\nid = \"review\"\nenabled = false\n",
    );
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    assert!(notices(&events).iter().any(|(level, text)| {
        *level == NoticeLevel::Info
            && text == "Review skipped for T1.1: the review stage is disabled."
    }));
    assert!(
        fixture
            .tasks_file()
            .contains("- [x] T1.1: [--.B-] add the greeting file")
    );
    // Only the builder session: the sprint config ends the session on the
    // emptied queue (T103.1).
    assert_eq!(provider.sessions().len(), 1);
}

#[tokio::test]
async fn batch_review_defers_every_task_but_the_last_and_the_last_diff_spans_the_group() {
    let tasks = "- [ ] T1.1: add the first file\n- [ ] T1.2: add the second file\n- [ ] T1.3: add the third file\n";
    let fixture = Fixture::new(tasks);
    let provider = MockProvider::per_session(vec![
        builder_with_claims("first.txt", "first\n", "none"),
        builder_with_claims("second.txt", "second\n", "none"),
        builder_with_claims("third.txt", "third\n", "none"),
        review_pass(),
    ]);
    // The simple-skip rule is off, so only the batch rule defers; the tasks'
    // clean builds would otherwise skip their reviews.
    let engine = fixture.engine_configured(provider.clone(), "skip_review_for_simple = false\n");
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The first two tasks defer their reviews with the recorded reason.
    let deferred =
        "Review skipped for T1.1: batch review defers the review to the group's last task.";
    let deferred2 =
        "Review skipped for T1.2: batch review defers the review to the group's last task.";
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Info && text == deferred)
    );
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Info && text == deferred2)
    );
    // Every task of the group committed individually as validated.
    let file = fixture.tasks_file();
    assert!(
        file.contains("- [x] T1.1: [--.B-] add the first file"),
        "{file}"
    );
    assert!(
        file.contains("- [x] T1.2: [--.B-] add the second file"),
        "{file}"
    );
    assert!(
        file.contains("- [x] T1.3: [--.BR] add the third file"),
        "{file}"
    );
    let log = fixture.git(&["log", "--format=%s"]);
    assert!(log.contains("feat(T1.1)"), "{log}");
    assert!(log.contains("feat(T1.2)"), "{log}");
    assert!(log.contains("feat(T1.3)"), "{log}");

    // One builder per task, then one reviewer session for the group's last
    // task, whose diff spans every commit of the group.
    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    // The three builders and the group's reviewer; the sprint config ends the
    // session on the emptied queue (T103.1).
    assert_eq!(labels, ["T1.1", "T1.2", "T1.3", "review"]);
    let reviewer_prompt = &sessions[3].prompt;
    assert!(
        reviewer_prompt.contains("first.txt"),
        "the group diff should span the first commit: {reviewer_prompt}"
    );
    assert!(reviewer_prompt.contains("second.txt"), "{reviewer_prompt}");
    assert!(reviewer_prompt.contains("third.txt"), "{reviewer_prompt}");
    // The last task's claims ride along.
    assert!(reviewer_prompt.contains("## Build Claims"));
}

#[tokio::test]
async fn a_multipass_review_reviews_each_file_then_integrates() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let per_file = |file: &str| {
        vec![Step::Event(AgentEvent::Result {
            text: format!(
                "Verdict: FAIL\n\n{{\"high\": [{{\"file\": \"{file}\", \"issue\": \"a leak\", \"fixed\": false, \"confidence\": 0.9}}], \"medium\": [], \"low\": []}}\n"
            ),
        })]
    };
    let provider = MockProvider::per_session(vec![
        builder_with_claims("a.txt", "a\n", "none"),
        per_file("a.txt"),
        per_file("b.txt"),
        per_file("c.txt"),
        review_pass(),
    ]);
    // Three changed files with a threshold of one: one reviewer per file,
    // then the integration pass. The builder writes one file; the other two
    // land as untracked files the way scaffolding tools leave them.
    let engine = fixture.engine_configured(
        provider.clone(),
        "skip_review_for_simple = false\nreview_multipass_threshold = 1\n",
    );
    // The project-local config file lives outside the worktree (in the data
    // dir), so only the written files count as changed.
    std::fs::write(fixture.path().join("b.txt"), "b\n").unwrap();
    std::fs::write(fixture.path().join("c.txt"), "c\n").unwrap();
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    let events = collect_until(&mut attachment.events, is_phase_startup).await;

    // The integration pass merged and de-duplicated the per-file findings
    // and produced the final report.
    let sessions = provider.sessions();
    let labels: Vec<_> = sessions.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["T1.1", "review", "review", "review", "review"]);
    for (session, file) in sessions[1..4].iter().zip(["a.txt", "b.txt", "c.txt"]) {
        assert!(
            session
                .prompt
                .contains(&format!("exactly this one: {file}"))
        );
        assert!(session.prompt.contains("report-only"));
    }
    let integration = &sessions[4].prompt;
    assert!(integration.contains("cross-file issues"));
    // Every per-file finding reached the integration prompt.
    assert_eq!(integration.matches("a leak").count(), 3, "{integration}");
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished {
            outcome: TaskOutcome::Done,
            ..
        }
    )));
}

// ---- Task injection while the engine is busy (T77.1) ----

/// Waits until the mock provider has recorded `count` sessions (each session
/// records itself the moment it starts), so a test can inject a task line
/// while a scripted session -- builder, reviewer or planner -- is mid-flight.
async fn wait_for_sessions(provider: &MockProvider, count: usize) {
    for _ in 0..500 {
        if provider.sessions().len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the session never started");
}

/// T77.1: a task line injected while a builder session is in progress is
/// present and unchanged after the current task commits, reached the queue
/// immediately (before the finish, without waiting for the file poll), and
/// is picked up and run as the next task without a restart.
#[tokio::test]
async fn a_task_injected_mid_build_runs_next_without_a_restart() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::new(vec![
        Step::Sleep(Duration::from_millis(300)),
        Step::WriteFile {
            path: PathBuf::from("greeting.txt"),
            contents: "hello\n".into(),
        },
    ]);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    wait_for_sessions(&provider, 1).await;
    engine
        .inject_task("- [ ] T2.1: injected while busy")
        .await
        .unwrap();

    // The first task commits with its progress token; the injected line is
    // still there, unchanged and unchecked.
    let events = collect_until(&mut attachment.events, |e| {
        matches!(
            e,
            EngineEvent::TaskFinished {
                outcome: TaskOutcome::Done,
                ..
            }
        )
    })
    .await;
    let file = fixture.tasks_file();
    assert!(file.contains("- [x] T1.1: [--.B-] add the greeting file\n"));
    assert!(
        file.contains("\n- [ ] T2.1: injected while busy\n"),
        "{file}"
    );
    // The line reached the queue before the finish: the inject's own
    // reconcile broadcast, not the 2 s file poll.
    let finish = events
        .iter()
        .position(|e| matches!(e, EngineEvent::TaskFinished { .. }))
        .unwrap();
    assert!(
        events[..finish].iter().any(|e| matches!(
            e,
            EngineEvent::TasksChanged { tasks }
                if tasks.iter().any(|t| t.id == "T2.1")
        )),
        "no task-list change carried the injected task before the finish"
    );

    // The queue picked the line up as the next task and ran it, with no
    // restart and nothing started mid-session.
    let events = collect_until(
        &mut attachment.events,
        |e| matches!(e, EngineEvent::TaskFinished { id, .. } if id == "T2.1"),
    )
    .await;
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskStarted { id, .. } if id == "T2.1"
    )));
    let file = fixture.tasks_file();
    // The injected task ran the research stage ("injected while busy" is not
    // a Simple description) before its builder session, exactly as a task
    // typed into the file by hand would.
    assert!(
        file.contains("- [x] T2.1: [R-.B-] injected while busy"),
        "{file}"
    );
    let labels: Vec<_> = provider
        .sessions()
        .iter()
        .map(|s| s.label.clone())
        .collect();
    // The injected task ran through the normal pipeline as the next task:
    // T1.1's builder, then (the description is not Simple) its research
    // session, then its builder session -- no restart, nothing mid-session.
    assert_eq!(
        &labels[..3],
        &[
            "T1.1".to_string(),
            "research".to_string(),
            "T2.1".to_string()
        ]
    );
}

/// T77.1: an inject racing the startup cleanup serializes on the task-file
/// lock: the pruning happens and the injected line survives verbatim.
#[tokio::test]
async fn an_inject_concurrent_with_the_startup_cleanup_survives_it() {
    let tasks = "- [x] T1.1: a\n- [x] T1.2: b\n- [x] T1.3: c\n- [x] T1.4: d\n\
                 - [x] T1.5: e\n- [x] T1.6: f\n- [x] T1.7: g\n- [x] T2.1: h\n\
                 - [x] T2.2: i\n- [x] T2.3: j\n- [x] T2.4: k\n- [ ] T3.1: pending\n";
    let fixture = Fixture::new(tasks);
    let engine = fixture.engine(MockProvider::new(vec![]));

    let line = "- [ ] T9.1: injected during the cleanup";
    tokio::join!(engine.cleanup_completed_tasks(), engine.inject_task(line),)
        .1
        .unwrap();

    let file = fixture.tasks_file();
    // The pruning happened: the old completed lines of the finished groups
    // outside the newest five are gone, the last group and its pending line
    // stay.
    assert!(!file.contains("- [x] T1.1: a\n"), "{file}");
    assert!(
        file.contains(
            "- [x] T1.7: g\n- [x] T2.1: h\n- [x] T2.2: i\n- [x] T2.3: j\n\
             - [x] T2.4: k\n- [ ] T3.1: pending\n"
        ),
        "{file}"
    );
    // The injected line survived byte for byte, and the file still parses.
    assert!(file.contains(&format!("{line}\n")), "{file}");
    let parsed = patok_engine::taskfile::load(&fixture.path().join(TASK_FILE)).unwrap();
    assert!(parsed.iter().any(|t| t.id == "T9.1" && !t.done));
}

/// T77.1: an inject during the reviewer session survives the review stage's
/// progress-indicator rewrite byte for byte.
#[tokio::test]
async fn an_inject_concurrent_with_the_review_indicator_write_survives_it() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::per_session(vec![
        builder_with_claims("greeting.txt", "hello\n", "none"),
        vec![
            Step::Sleep(Duration::from_millis(300)),
            Step::Event(AgentEvent::Result {
                text: "The claims check out.\n\nVerdict: PASS\n\n{\"high\": [], \"medium\": [], \"low\": []}\n"
                    .into(),
            }),
        ],
    ]);
    let engine = fixture.engine_configured(provider.clone(), "skip_review_for_simple = false\n");
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    // The inject lands while the reviewer session is mid-flight; the loop
    // then soft-stops, so the injected task is only asserted on, not run.
    wait_for_sessions(&provider, 2).await;
    engine
        .inject_task("- [ ] T9.1: injected during the review")
        .await
        .unwrap();
    engine.request_stop(false);

    let events = collect_until(&mut attachment.events, |e| {
        matches!(
            e,
            EngineEvent::TaskFinished {
                outcome: TaskOutcome::Done,
                ..
            }
        )
    })
    .await;
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TaskFinished { id, outcome: TaskOutcome::Done, commit: Some(_) }
            if id == "T1.1"
    )));
    let file = fixture.tasks_file();
    // The validated progress token landed on T1.1 and the injected line
    // survived the rewrite untouched.
    assert!(
        file.contains("- [x] T1.1: [--.BR] add the greeting file\n"),
        "{file}"
    );
    assert!(
        file.contains("\n- [ ] T9.1: injected during the review\n"),
        "{file}"
    );
}

/// T77.1: several timed injects interleaved with a running build all land
/// intact, in order, with no corruption and no temporary file left behind.
#[tokio::test]
async fn timed_injects_interleaved_with_a_run_all_land_intact_in_order() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let mut script: Vec<Step> = (0..5)
        .map(|_| Step::Sleep(Duration::from_millis(100)))
        .collect();
    script.push(Step::WriteFile {
        path: PathBuf::from("greeting.txt"),
        contents: "hello\n".into(),
    });
    let provider = MockProvider::new(script);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_build().unwrap();
    // The loop soft-stops after T1.1, so the injected lines stay pending and
    // the run ends with the original task committed.
    engine.request_stop(false);
    let injects = engine.clone();
    let spawned = tokio::spawn(async move {
        for n in 1..=5 {
            tokio::time::sleep(Duration::from_millis(60)).await;
            injects
                .inject_task(&format!("- [ ] T9.{n}: injected line {n}"))
                .await
                .unwrap();
        }
    });

    let _events = collect_until(&mut attachment.events, is_phase_startup).await;
    spawned.await.unwrap();

    let file = fixture.tasks_file();
    assert!(
        file.contains("- [x] T1.1: [--.B-] add the greeting file\n"),
        "{file}"
    );
    // Every injected line is intact, in order, exactly once, and never
    // joined with another.
    let mut last = None;
    for n in 1..=5 {
        let line = format!("- [ ] T9.{n}: injected line {n}\n");
        let at = file
            .find(&line)
            .unwrap_or_else(|| panic!("missing {line:?}: {file}"));
        assert_eq!(
            file.matches(&line).count(),
            1,
            "{line:?} appears once: {file}"
        );
        if let Some(last) = last {
            assert!(at > last, "the injected lines are out of order: {file}");
        }
        last = Some(at);
    }
    assert_eq!(
        file.lines().filter(|l| l.starts_with("- [ ] T9.")).count(),
        5,
        "{file}"
    );
    // The file parses, no line is malformed, and the run ended after T1.1.
    let parsed = patok_core::task::parse(&file);
    assert_eq!(parsed.iter().filter(|t| t.is_malformed()).count(), 0);
    assert_eq!(parsed.iter().filter(|t| !t.done).count(), 5);
    let labels: Vec<_> = provider
        .sessions()
        .iter()
        .map(|s| s.label.clone())
        .collect();
    assert_eq!(labels, ["T1.1".to_string()]);
    // No temporary rewrite file was left behind.
    assert!(!fixture.path().join("TASKS.md.patok-tmp").exists());
}

/// T77.1: a line injected during a planner session survives a rejected run's
/// restore: the file is `before` plus the injected line, re-appended
/// verbatim at the end.
#[tokio::test]
async fn an_inject_during_a_planner_session_survives_a_rejected_restore() {
    let fixture = Fixture::new(TASKS);
    let before = fixture.tasks_file();
    // The planner rewrites the file, checking an existing task: a violation.
    let violating = before.replace("[ ] T1.1", "[x] T1.1");
    let provider = MockProvider::new(vec![
        Step::Sleep(Duration::from_millis(300)),
        Step::WriteFile {
            path: PathBuf::from("TASKS.md"),
            contents: violating,
        },
    ]);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_add_tasks("plan more tasks").unwrap();
    wait_for_sessions(&provider, 1).await;
    engine
        .inject_task("- [ ] T9.1: injected during the planner")
        .await
        .unwrap();
    let events = collect_until(&mut attachment.events, planning_finished).await;

    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Error
                && text.starts_with("planner result rejected:")),
        "{events:#?}"
    );
    assert_eq!(
        fixture.tasks_file(),
        format!("{before}- [ ] T9.1: injected during the planner\n")
    );
}

/// T77.1: a valid planner append plus an inject passes the validation and
/// reports only the planner's count; the injected line is not the planner's
/// doing and stays in the file and the queue.
#[tokio::test]
async fn an_inject_during_a_planner_session_counts_only_the_planners_tasks() {
    let fixture = Fixture::new(TASKS);
    let before = fixture.tasks_file();
    let injected = "- [ ] T8.1: injected during the planner\n";
    let after = format!("{before}{injected}- [ ] T9.1: planned by the planner\n");
    let provider = MockProvider::new(vec![
        Step::Sleep(Duration::from_millis(300)),
        Step::WriteFile {
            path: PathBuf::from("TASKS.md"),
            contents: after.clone(),
        },
    ]);
    let engine = fixture.engine(provider.clone());
    let mut attachment = engine.attach().unwrap();

    engine.start_add_tasks("plan more tasks").unwrap();
    wait_for_sessions(&provider, 1).await;
    engine
        .inject_task("- [ ] T8.1: injected during the planner")
        .await
        .unwrap();
    let events = collect_until(&mut attachment.events, planning_finished).await;

    // The session passed and counts only its own task.
    assert!(
        notices(&events)
            .iter()
            .any(|(level, text)| *level == NoticeLevel::Info && text == "1 task added."),
        "{events:#?}"
    );
    assert_eq!(fixture.tasks_file(), after);
    assert!(events.iter().any(|e| matches!(
        e,
        EngineEvent::TasksChanged { tasks }
            if tasks.iter().any(|t| t.id == "T8.1")
                && tasks.iter().any(|t| t.id == "T9.1")
    )));
    assert_eq!(
        provider
            .sessions()
            .iter()
            .map(|s| s.label.as_str())
            .collect::<Vec<_>>(),
        ["planner"]
    );
}

/// T77.1: the confirmed inject text is normalized into a well-formed
/// unchecked task line -- plain text gains the checkbox and the next
/// unused `T` id, an already formatted line lands unchanged -- with the
/// existing lines byte-identical, and no agent session is involved.
#[tokio::test]
async fn inject_task_normalizes_plain_text_into_a_well_formed_line() {
    let fixture = Fixture::new("- [ ] T1.1: add the greeting file\n");
    let provider = MockProvider::new(vec![]);
    let engine = fixture.engine(provider.clone());
    let before = fixture.tasks_file();

    // Plain text gains the checkbox and the next id after the file's
    // highest (T1), leaving the existing line byte-identical.
    engine.inject_task("fix the greeting flow").await.unwrap();
    assert_eq!(
        fixture.tasks_file(),
        format!("{before}- [ ] T2.1: fix the greeting flow\n")
    );

    // A second inject reads the file under the same lock and gets the next
    // number; a checkbox-only input gains just the id.
    engine.inject_task("  - [ ] polish it ").await.unwrap();
    assert_eq!(
        fixture.tasks_file(),
        format!("{before}- [ ] T2.1: fix the greeting flow\n- [ ] T3.1: polish it\n")
    );

    // An already formatted line lands unchanged, whitespace-trimmed only.
    engine
        .inject_task("  - [ ] T9.1: already formatted  ")
        .await
        .unwrap();
    assert_eq!(
        fixture.tasks_file(),
        format!(
            "{before}- [ ] T2.1: fix the greeting flow\n\
             - [ ] T3.1: polish it\n- [ ] T9.1: already formatted\n"
        )
    );

    // A whitespace-only input is refused and the file is untouched.
    let after = fixture.tasks_file();
    assert_eq!(
        engine.inject_task("   ").await,
        Err("the task line is empty".into())
    );
    assert_eq!(fixture.tasks_file(), after);
    // A multi-line input (the modal's Shift-Enter or a paste) is refused
    // the same way, so no raw text line lands in TASKS.md; only the
    // trailing line break of an otherwise single-line text is tolerated.
    assert_eq!(
        engine.inject_task("two\nlines").await,
        Err("the task line must be a single line".into())
    );
    assert_eq!(fixture.tasks_file(), after);
    // Only a trailing line break is tolerated: the trimmed text is one
    // line and lands as such, with the next id after the highest `T`
    // number now in the file (the injected T9.1).
    engine.inject_task("only one line\n").await.unwrap();
    assert_eq!(
        fixture.tasks_file(),
        format!("{after}- [ ] T10.1: only one line\n")
    );
    // No agent session, planner or discovery was started by any inject.
    assert!(provider.sessions().is_empty());
}
