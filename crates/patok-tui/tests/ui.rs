//! Screen snapshots rendered with ratatui's `TestBackend`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use patok_core::event::{
    AgentEvent, EngineEvent, Phase, SessionOutcome, Snapshot, TaskOutcome, Usage,
};
use patok_core::pipeline::PipelineState;
use patok_core::task;
use patok_tui::{
    App, finished_line, format_elapsed, human_duration, output_title, output_title_parts,
    output_title_spans, render, started_line,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const TASKS: &str = "## Phase 1\n- [x] T1.1: scaffold the workspace\n- [ ] T1.2: add the parser for task files\n- [ ] T1.3: wire the engine to the shell\n";

fn app() -> App {
    App::new(
        Snapshot {
            project_dir: "/home/user/demo".into(),
            phase: Phase::Startup,
            tasks: task::parse(TASKS),
            current_task: None,
            planning: false,
            discovering: false,
            provider: String::new(),
            model: String::new(),
            settings: BTreeMap::new(),
            pipeline: PipelineState::today(),
            recent: vec![],
        },
        "0.1.0".into(),
    )
}

fn draw(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn agent(event: AgentEvent) -> EngineEvent {
    EngineEvent::Agent { event }
}

/// Renders into an 80x24 TestBackend and returns the buffer, for cell-level
/// symbol and style assertions.
fn draw_buffer(app: &App) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn idle_dashboard() {
    let app = app();
    insta::assert_snapshot!(draw(&app, 80, 14));
}

/// The idle strip's other face (T46.1): a complete queue points Enter at a
/// discovery round instead of the build loop.
#[test]
fn idle_dashboard_with_a_complete_queue() {
    let mut app = app();
    app.apply(EngineEvent::TasksChanged {
        tasks: task::parse(&TASKS.replace("[ ]", "[x]")),
    });
    assert!(app.tasks.iter().all(|t| t.done));
    insta::assert_snapshot!(draw(&app, 80, 14));
}

/// The tasks frame title reads ` Tasks | done/total - N left ` with only the
/// word `Tasks` in the frame's title colour and everything after it -- the
/// pipe separator, the three counts, the slash, the dash and the word `left`
/// -- in the theme's low-emphasis detail colour (T67.1); the counts track the
/// live task state and the split holds in every theme.
#[test]
fn tasks_frame_title_counts_and_colours() {
    use patok_core::config::Theme as ThemeKey;
    use patok_tui::Theme;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    /// Asserts the cells from (x, y) spell `text` in `expected`.
    fn assert_segment(buffer: &Buffer, x: u16, y: u16, text: &str, expected: Color) {
        for (i, ch) in text.chars().enumerate() {
            let cell = &buffer[(x + i as u16, y)];
            assert_eq!(
                cell.symbol(),
                ch.to_string(),
                "cell ({}, {})",
                x + i as u16,
                y
            );
            assert_eq!(
                cell.style().fg,
                Some(expected),
                "cell ({}, {})",
                x + i as u16,
                y
            );
        }
    }

    /// Draws `app` and checks its tasks frame title reads
    /// ` Tasks | done/total - N left ` with the word `Tasks` in the frame's
    /// title colour (`theme.foreground`, what the base-style paint leaves
    /// unstyled title cells with) and every other part in `detail`.
    fn assert_title(app: &App, done: usize, total: usize, left: usize, theme: Theme) {
        let buffer = draw_buffer(app);
        let tasks = app.tasks_area.get();
        let (x, y) = (tasks.x + 1, tasks.y);
        let (title, detail) = (theme.foreground, theme.muted_text);
        assert_segment(&buffer, x, y, " Tasks", title);
        let mut x = x + " Tasks".len() as u16;
        assert_segment(&buffer, x, y, " | ", detail);
        x += " | ".len() as u16;
        assert_segment(&buffer, x, y, &done.to_string(), detail);
        x += done.to_string().len() as u16;
        assert_segment(&buffer, x, y, "/", detail);
        x += 1;
        assert_segment(&buffer, x, y, &total.to_string(), detail);
        x += total.to_string().len() as u16;
        assert_segment(&buffer, x, y, " - ", detail);
        x += " - ".len() as u16;
        assert_segment(&buffer, x, y, &left.to_string(), detail);
        x += left.to_string().len() as u16;
        assert_segment(&buffer, x, y, " left ", detail);
    }

    // Mixed queue: one of three done, so two are left; only the word `Tasks`
    // keeps the title colour and everything after it is gray in the default
    // theme.
    assert_title(&app(), 1, 3, 2, Theme::DARK);
    assert!(draw(&app(), 80, 24).contains(" Tasks | 1/3 - 2 left "));

    // Completing every task drops the left count to zero on the next frame...
    let mut complete = app();
    complete.apply(EngineEvent::TasksChanged {
        tasks: task::parse(&TASKS.replace("[ ]", "[x]")),
    });
    assert_title(&complete, 3, 3, 0, Theme::DARK);

    // ...and uncompleting one brings it back through the same render path.
    let mut uncompleted = complete;
    uncompleted.apply(EngineEvent::TasksChanged {
        tasks: task::parse(TASKS),
    });
    assert_title(&uncompleted, 1, 3, 2, Theme::DARK);

    // An empty queue reads zero, zero and zero and keeps its placeholder.
    let mut empty = app();
    empty.apply(EngineEvent::TasksChanged { tasks: vec![] });
    assert_title(&empty, 0, 0, 0, Theme::DARK);
    assert!(draw(&empty, 80, 24).contains("no tasks in TASKS.md"));

    // The split holds under palette themes too: the muted role recolours with
    // the active theme instead of a fixed literal, in both a dark and a light
    // palette.
    let mut themed = app();
    themed.tui.theme = ThemeKey::TokyoNightDark;
    themed.tui.truecolor = Some(true);
    let palette = Theme::resolve(ThemeKey::TokyoNightDark, Some(true));
    assert_ne!(palette.muted_text, Theme::DARK.muted_text);
    assert_ne!(palette.foreground, Theme::DARK.foreground);
    assert_title(&themed, 1, 3, 2, palette);

    let mut light = app();
    light.tui.theme = ThemeKey::CatppuccinLatte;
    light.tui.truecolor = Some(true);
    let resolved = Theme::resolve(ThemeKey::CatppuccinLatte, Some(true));
    assert_ne!(resolved.muted_text, palette.muted_text);
    assert_ne!(resolved.foreground, palette.foreground);
    assert_title(&light, 1, 3, 2, resolved);
}

#[test]
fn running_with_streamed_output() {
    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::TaskStarted {
        id: "T1.2".into(),
        description: "add the parser for task files".into(),
    });
    app.apply(agent(AgentEvent::TextDelta {
        text: "I'll read the existing ".into(),
    }));
    app.apply(agent(AgentEvent::TextDelta {
        text: "code first.\nThen write the parser.".into(),
    }));
    app.apply(agent(AgentEvent::ToolUse {
        name: "Read".into(),
        input: "src/lib.rs".into(),
    }));
    app.apply(agent(AgentEvent::ToolResult {
        output: "pub mod task;".into(),
    }));
    app.apply(agent(AgentEvent::Usage(Usage {
        input_tokens: 50_000,
        context_window: 200_000,
        ..Usage::default()
    })));
    insta::assert_snapshot!(draw(&app, 80, 14));
}

#[test]
fn finished_task_is_ticked_and_logged() {
    let mut app = app();
    app.apply(EngineEvent::TasksChanged {
        tasks: task::parse(&TASKS.replace("[ ] T1.2", "[x] T1.2")),
    });
    app.apply(EngineEvent::TaskFinished {
        id: "T1.2".into(),
        outcome: TaskOutcome::Done,
        commit: Some("abc1234".into()),
    });
    assert_eq!(app.completed, 1);
    insta::assert_snapshot!(draw(&app, 80, 14));
}

#[test]
fn narrow_terminal_wraps_output() {
    let mut app = app();
    app.apply(agent(AgentEvent::Text {
        text: "a rather long line of agent output that must wrap in a narrow pane".into(),
    }));
    insta::assert_snapshot!(draw(&app, 40, 12));
}

#[test]
fn header_shows_only_the_status_chip_and_run_mode() {
    let mut app = app();
    let idle = draw(&app, 80, 14);
    // The merged status line (T86.1): the chips on the left, the key hints
    // right-aligned behind them. At 80 columns the idle line fits whole; the
    // shorter strip the m menu leaves (T128.1) right-aligns its two pairs.
    assert_eq!(
        idle.lines().last().unwrap(),
        format!(" STOPPED  sprint {}Enter  start  m  menu", " ".repeat(41))
    );
    assert!(!idle.contains("Waiting..."));
    // The body starts on the frame's first row (the header row is gone,
    // T86.1): the rail column opens with its blank top row (T63.1) beside
    // the frames' top border, and the first tile sits one row lower.
    assert!(
        idle.lines()
            .next()
            .unwrap()
            .trim_start()
            .starts_with("┌ Builder")
    );
    assert!(idle.lines().nth(1).unwrap().starts_with("[ Research ]"));

    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::TaskStarted {
        id: "T1.2".into(),
        description: "add the parser for task files".into(),
    });
    app.apply(agent(AgentEvent::Usage(Usage {
        input_tokens: 50_000,
        context_window: 200_000,
        ..Usage::default()
    })));
    let running = draw(&app, 80, 14);
    let status = running.lines().last().unwrap();
    assert!(status.starts_with(" RUNNING  sprint"), "{status}");
    assert!(!status.contains("builder"));
    assert!(!status.contains("context"));
}

#[test]
fn header_and_frame_title_show_the_active_agent_of_the_plan_stage() {
    let mut app = app();
    app.provider = "claude".into();
    app.model = "claude-sonnet-5.5".into();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::TaskStarted {
        id: "T1.2".into(),
        description: "add the parser for task files".into(),
    });
    app.apply(EngineEvent::AgentChanged {
        agent: "planner".into(),
        started_ms: 0,
    });
    app.apply(agent(AgentEvent::Thinking {
        text: "## Plan\nRead `task.rs` first.".into(),
    }));
    let planner_screen = draw(&app, 80, 14);
    assert!(
        planner_screen
            .lines()
            .last()
            .unwrap()
            .starts_with(" RUNNING  sprint")
    );
    assert!(planner_screen.contains(" Planner | Claude - claude-sonnet-5.5 "));
    // The plan output keeps its markdown rendering.
    assert!(planner_screen.contains("Plan"));
    assert!(planner_screen.contains("Read task.rs first."));
    insta::assert_snapshot!(planner_screen);

    app.apply(EngineEvent::AgentChanged {
        agent: "builder".into(),
        started_ms: 0,
    });
    let builder_screen = draw(&app, 80, 14);
    assert!(
        builder_screen
            .lines()
            .last()
            .unwrap()
            .starts_with(" RUNNING  sprint")
    );
    assert!(builder_screen.contains(" Builder | Claude - claude-sonnet-5.5 "));
    insta::assert_snapshot!(builder_screen);
}

/// The research stage surfaces in the shell like the plan stage (T68.1): the
/// status line's chips and the output frame title show the research agent with the provider
/// and model while the session runs.
#[test]
fn header_and_frame_title_show_the_research_agent_of_the_research_stage() {
    let mut app = app();
    app.provider = "claude".into();
    app.model = "claude-sonnet-5.5".into();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::TaskStarted {
        id: "T1.2".into(),
        description: "add the parser for task files".into(),
    });
    app.apply(EngineEvent::AgentChanged {
        agent: "research".into(),
        started_ms: 0,
    });
    app.apply(agent(AgentEvent::Thinking {
        text: "## Investigation\nWhat does the parser do today?".into(),
    }));
    let screen = draw(&app, 80, 14);
    assert!(
        screen
            .lines()
            .last()
            .unwrap()
            .starts_with(" RUNNING  sprint")
    );
    assert!(screen.contains(" Research | Claude - claude-sonnet-5.5 "));
    assert!(screen.contains("What does the parser do today?"));
    insta::assert_snapshot!(screen);
}

/// The review stage surfaces in the shell like the plan stage (T70.1): the
/// status line's chips and the output frame title show the reviewer agent with the
/// provider and model while the session runs.
#[test]
fn header_and_frame_title_show_the_reviewer_agent_of_the_review_stage() {
    let mut app = app();
    app.provider = "claude".into();
    app.model = "claude-sonnet-5.5".into();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::TaskStarted {
        id: "T1.2".into(),
        description: "add the parser for task files".into(),
    });
    app.apply(EngineEvent::AgentChanged {
        agent: "builder".into(),
        started_ms: 0,
    });
    app.apply(EngineEvent::AgentChanged {
        agent: "reviewer".into(),
        started_ms: 0,
    });
    app.apply(agent(AgentEvent::Thinking {
        text: "## Review\nVerifying the builder's claims.".into(),
    }));
    let screen = draw(&app, 80, 14);
    assert!(
        screen
            .lines()
            .last()
            .unwrap()
            .starts_with(" RUNNING  sprint")
    );
    assert!(screen.contains(" Reviewer | Claude - claude-sonnet-5.5 "));
    assert!(screen.contains("Verifying the builder's claims."));
    insta::assert_snapshot!(screen);
}

/// A reviewer session that runs on another provider than the configured one
/// (a claude reviewer provider under a mistral provider, T81.1) titles the output
/// frame with that provider and the reviewer model from its starting event on:
/// the configured provider never silently stands in. The configured pair comes
/// back when the session ends, and a builder session keeps its own pair.
#[test]
fn a_reviewer_session_started_switches_the_frame_title_to_its_provider_and_model() {
    let mut app = app();
    app.provider = "mistral".into();
    app.config_provider = app.provider.clone();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::TaskStarted {
        id: "T1.2".into(),
        description: "add the parser for task files".into(),
    });
    app.apply(EngineEvent::AgentStarted {
        agent: "reviewer".into(),
        provider: "claude".into(),
        model: Some("opus".into()),
    });
    app.apply(EngineEvent::AgentChanged {
        agent: "reviewer".into(),
        started_ms: 0,
    });
    app.apply(agent(AgentEvent::Thinking {
        text: "## Review\nVerifying the builder's claims.".into(),
    }));
    let screen = draw(&app, 80, 14);
    assert!(screen.contains(" Reviewer | Claude - opus "), "{screen}");
    assert!(!screen.contains("Mistral"), "{screen}");
    insta::assert_snapshot!(screen);

    // The session ends: the title returns to the configured provider, never
    // keeping the reviewer's provider or model.
    app.apply(EngineEvent::AgentFinished {
        agent: "reviewer".into(),
        outcome: SessionOutcome::Finished,
        duration_ms: 45_000,
    });
    let screen = draw(&app, 80, 14);
    assert!(!screen.contains("Claude - opus"), "{screen}");
    assert!(screen.contains(" Reviewer | Mistral "), "{screen}");

    // A builder session still shows its own provider and model.
    app.apply(EngineEvent::AgentStarted {
        agent: "builder".into(),
        provider: "mistral".into(),
        model: Some("mistral-small".into()),
    });
    app.apply(EngineEvent::AgentChanged {
        agent: "builder".into(),
        started_ms: 0,
    });
    let screen = draw(&app, 80, 14);
    assert!(
        screen.contains(" Builder | Mistral - mistral-small "),
        "{screen}"
    );
}

#[test]
fn replayed_events_set_the_active_agent() {
    let snapshot = Snapshot {
        project_dir: "/p".into(),
        phase: Phase::Running,
        tasks: task::parse(TASKS),
        current_task: Some("T1.2".into()),
        planning: false,
        discovering: false,
        provider: String::new(),
        model: String::new(),
        settings: BTreeMap::new(),
        pipeline: PipelineState::today(),
        recent: vec![
            EngineEvent::AgentChanged {
                agent: "planner".into(),
                started_ms: 0,
            },
            agent(AgentEvent::Text {
                text: "planning".into(),
            }),
        ],
    };
    let app = App::new(snapshot, "0.1.0".into());
    assert_eq!(app.agent, "planner");
    assert!(
        draw(&app, 80, 12)
            .lines()
            .next()
            .unwrap()
            .contains("┌ Planner")
    );
}

#[test]
fn narrow_header_drops_the_run_mode_chip() {
    // 18 columns fit both chips and no hint pair.
    let screen = draw(&app(), 18, 10);
    assert_eq!(screen.lines().last().unwrap(), " STOPPED  sprint");
    insta::assert_snapshot!(screen);
    // 12 columns fit only the status chip; the run-mode chip goes next.
    let screen = draw(&app(), 12, 10);
    assert_eq!(screen.lines().last().unwrap(), " STOPPED");
    insta::assert_snapshot!(screen);
    // 60 columns keep both chips and every hint pair: the secondary hints
    // moved behind the m menu (T128.1), so the two pairs that remain -- the
    // idle Enter hint and the menu chip -- fit whole, right-aligned.
    let screen = draw(&app(), 60, 12);
    let status = screen.lines().last().unwrap();
    assert_eq!(
        status,
        format!(" STOPPED  sprint {}Enter  start  m  menu", " ".repeat(21))
    );
    assert!(!status.contains(" detach"), "{status}");
    assert!(!status.contains(" quit"), "{status}");
    // 40 columns fit both remaining pairs whole: the shorter Enter hint
    // (T129.1) lets the menu pair join the line, right-aligned.
    let screen = draw(&app(), 40, 12);
    assert_eq!(
        screen.lines().last().unwrap(),
        " STOPPED  sprint  Enter  start  m  menu"
    );
}

#[test]
fn output_title_keeps_everything_when_wide_and_drops_parts_when_narrow() {
    let full = " Builder | Claude - claude-sonnet-5.5 ";
    let wide = full.chars().count();
    assert_eq!(
        output_title("Builder", "Claude", "claude-sonnet-5.5", wide),
        full
    );
    assert_eq!(
        output_title("Builder", "Claude", "claude-sonnet-5.5", 100),
        full
    );
    // One column short: the model goes first.
    assert_eq!(
        output_title("Builder", "Claude", "claude-sonnet-5.5", wide - 1),
        " Builder | Claude "
    );
    // Then the provider name.
    assert_eq!(
        output_title(
            "Builder",
            "Claude",
            "claude-sonnet-5.5",
            " Builder | Claude ".chars().count() - 1
        ),
        " Builder "
    );
    // The agent type never goes away.
    assert_eq!(
        output_title("Builder", "Claude", "claude-sonnet-5.5", 0),
        " Builder "
    );
    // Without a known model the provider-only form is the full title.
    assert_eq!(
        output_title("Builder", "Mistral", "", 20),
        " Builder | Mistral "
    );
    assert_eq!(
        output_title(
            "Builder",
            "Mistral",
            "",
            " Builder | Mistral ".chars().count() - 1
        ),
        " Builder "
    );
    // Without a provider there is only the agent type.
    assert_eq!(
        output_title("Builder", "", "claude-sonnet-5.5", 100),
        " Builder "
    );
}

#[test]
fn output_title_parts_drop_the_model_then_the_provider_then_the_timer() {
    let full = " Builder | Claude - claude-sonnet-5.5 ";
    let timer = " 12:34 ";
    // Wide: everything fits, the timer on the right.
    assert_eq!(
        output_title_parts("Builder", "Claude", "claude-sonnet-5.5", Some("12:34"), 100),
        (full.to_string(), timer.to_string())
    );
    // One column short of the full title plus the timer: the model goes first.
    let wide = full.chars().count() + timer.chars().count();
    assert_eq!(
        output_title_parts(
            "Builder",
            "Claude",
            "claude-sonnet-5.5",
            Some("12:34"),
            wide - 1
        ),
        (" Builder | Claude ".to_string(), timer.to_string())
    );
    // Then the provider; the agent type keeps its timer.
    let with_provider = " Builder | Claude ".chars().count();
    assert_eq!(
        output_title_parts(
            "Builder",
            "Claude",
            "claude-sonnet-5.5",
            Some("12:34"),
            with_provider + timer.chars().count() - 1
        ),
        (" Builder ".to_string(), timer.to_string())
    );
    // Just above the agent-plus-timer threshold both stay.
    assert_eq!(
        output_title_parts(
            "Builder",
            "Claude",
            "claude-sonnet-5.5",
            Some("12:34"),
            " Builder ".chars().count() + timer.chars().count()
        ),
        (" Builder ".to_string(), timer.to_string())
    );
    // Below it the timer goes, the agent type stays.
    assert_eq!(
        output_title_parts(
            "Builder",
            "Claude",
            "claude-sonnet-5.5",
            Some("12:34"),
            " Builder ".chars().count() + timer.chars().count() - 1
        ),
        (" Builder ".to_string(), String::new())
    );
    // Very narrow: never an empty left side, no timer.
    assert_eq!(
        output_title_parts("Builder", "Claude", "claude-sonnet-5.5", Some("12:34"), 0),
        (" Builder ".to_string(), String::new())
    );
}

#[test]
fn output_title_parts_without_a_timer_reproduce_the_plain_title() {
    let full = " Builder | Claude - claude-sonnet-5.5 ";
    for (provider, model, width, expected) in [
        ("Claude", "claude-sonnet-5.5", 100, full),
        (
            "Claude",
            "claude-sonnet-5.5",
            full.chars().count() - 1,
            " Builder | Claude ",
        ),
        ("Claude", "claude-sonnet-5.5", 9, " Builder "),
        ("Mistral", "", 20, " Builder | Mistral "),
        ("", "claude-sonnet-5.5", 100, " Builder "),
    ] {
        assert_eq!(
            output_title_parts("Builder", provider, model, None, width),
            (expected.to_string(), String::new())
        );
        assert_eq!(output_title("Builder", provider, model, width), expected);
    }
    // Without a provider the timer still shows next to the agent type.
    assert_eq!(
        output_title_parts("Builder", "", "claude-sonnet-5.5", Some("12:34"), 100),
        (" Builder ".to_string(), " 12:34 ".to_string())
    );
}

/// One title segment of row 0 (the output frame's top border): every cell shows
/// `text` in `colour`, bold only when the agent name is under test.
#[test]
fn output_frame_title_colours_only_the_agent_type() {
    use patok_tui::Theme;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier};

    fn assert_segment(buffer: &Buffer, x: u16, text: &str, colour: Color, bold: bool) {
        for (i, ch) in text.chars().enumerate() {
            let cell = &buffer[(x + i as u16, 0)];
            assert_eq!(cell.symbol(), ch.to_string(), "cell ({}, 0)", x + i as u16);
            assert_eq!(
                cell.style().fg,
                Some(colour),
                "cell ({}, 0) colour: {:?}",
                x + i as u16,
                cell.style()
            );
            assert_eq!(
                cell.style().add_modifier.contains(Modifier::BOLD),
                bold,
                "cell ({}, 0) bold: {:?}",
                x + i as u16,
                cell.style()
            );
        }
    }

    let mut app = app();
    app.provider = "claude".into();
    app.model = "claude-sonnet-5.5".into();
    let t0 = Instant::now();
    app.now.set(t0);
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    // A frozen clock advanced by hand makes the timer deterministic.
    app.now.set(t0 + Duration::from_secs(65));

    let detail = Theme::DARK.muted_text;
    // Builder: the name carries the builder colour and bold; the separator,
    // provider, model and the right-aligned timer are all gray. The run-active
    // rail (T50.1) shifts the frame right, so the title's columns derive from
    // the recorded frame rect.
    let buffer = draw_buffer(&app);
    let x = app.output_area.get().x + 1;
    assert_segment(
        &buffer,
        x,
        " Builder",
        Theme::agent_name_color(Theme::DARK, "builder"),
        true,
    );
    assert_segment(&buffer, x + 8, " | Claude", detail, false);
    assert_segment(&buffer, x + 17, " - claude-sonnet-5.5 ", detail, false);
    // The timer stays anchored to the frame's right edge, which the rail does
    // not move.
    let timer = app.output_area.get().right() - 8;
    assert_segment(&buffer, timer, " 01:05 ", detail, false);

    // Planner: only the name's colour changes; the same gray covers the rest.
    app.apply(EngineEvent::AgentChanged {
        agent: "planner".into(),
        started_ms: 0,
    });
    let buffer = draw_buffer(&app);
    let x = app.output_area.get().x + 1;
    assert_segment(
        &buffer,
        x,
        " Planner",
        Theme::agent_name_color(Theme::DARK, "planner"),
        true,
    );
    assert_segment(&buffer, x + 8, " | Claude", detail, false);
    assert_segment(&buffer, x + 17, " - claude-sonnet-5.5 ", detail, false);
    // The timer stays anchored to the frame's right edge, which the rail does
    // not move.
    let timer = app.output_area.get().right() - 8;
    assert_segment(&buffer, timer, " 01:05 ", detail, false);
}

#[test]
fn output_title_spans_concatenate_to_the_string_parts_and_split_the_colours() {
    use ratatui::style::{Color, Modifier};
    use ratatui::text::Span;

    let agent_colour = Color::Green;
    let detail_colour = Color::DarkGray;
    for (provider, model) in [
        ("Claude", "claude-sonnet-5.5"),
        ("Mistral", ""),
        ("", "claude-sonnet-5.5"),
    ] {
        for timer in [None, Some("12:34")] {
            for width in 0..=60 {
                let (left_spans, right_spans) = output_title_spans(
                    "Builder",
                    provider,
                    model,
                    timer,
                    width,
                    agent_colour,
                    detail_colour,
                );
                let (left, right) = output_title_parts("Builder", provider, model, timer, width);
                let text = |spans: &[Span<'_>]| {
                    spans
                        .iter()
                        .map(|span| span.content.clone().into_owned())
                        .collect::<String>()
                };
                assert_eq!(
                    text(&left_spans),
                    left,
                    "{provider}/{model}/timer {timer:?}/width {width}"
                );
                assert_eq!(
                    text(&right_spans),
                    right,
                    "{provider}/{model}/timer {timer:?}/width {width}"
                );
                // The first span is the agent type name: agent colour and bold.
                let first = &left_spans[0];
                assert_eq!(first.style.fg, Some(agent_colour));
                assert!(first.style.add_modifier.contains(Modifier::BOLD));
                // Every other span is low-emphasis detail: gray, no bold.
                for span in left_spans.iter().skip(1).chain(right_spans.iter()) {
                    assert_eq!(span.style.fg, Some(detail_colour));
                    assert!(!span.style.add_modifier.contains(Modifier::BOLD));
                }
            }
        }
    }
}

#[test]
fn format_elapsed_pads_minutes_and_seconds() {
    assert_eq!(format_elapsed(Duration::from_secs(0)), "00:00");
    assert_eq!(format_elapsed(Duration::from_secs(9)), "00:09");
    assert_eq!(format_elapsed(Duration::from_secs(65)), "01:05");
    assert_eq!(format_elapsed(Duration::from_secs(75 * 60 + 30)), "75:30");
    assert_eq!(format_elapsed(Duration::from_secs(3600)), "60:00");
}

#[test]
fn the_timer_renders_right_aligned_on_the_title_line_while_a_session_runs() {
    let mut app = app();
    // The title's truncation thresholds are the pane's widths, so pin the
    // narrow compact rail: this test probes the title, not the rail mode.
    app.tui.rail_mode = patok_core::config::RailMode::Compact;
    let t0 = Instant::now();
    app.now.set(t0);
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    // A frozen clock advanced by hand makes the timer deterministic.
    app.now.set(t0 + Duration::from_secs(65));
    let screen = draw(&app, 80, 14);
    let title = screen.lines().next().unwrap();
    // A run is active, so the rail's Plan tile precedes the frame border.
    assert!(title.contains("┌ Builder"), "{title}");
    assert!(
        title.ends_with(" 01:05 ┐"),
        "the timer is right-aligned: {title}"
    );
    // Narrowing drops the model first, then the provider; the timer survives both.
    app.provider = "claude".into();
    app.model = "claude-sonnet-5.5".into();
    let title = draw(&app, 40, 12).lines().next().unwrap().to_string();
    assert!(title.contains(" Builder | Claude "), "{title}");
    assert!(title.ends_with(" 01:05 ┐"), "{title}");
    let title = draw(&app, 26, 10).lines().next().unwrap().to_string();
    assert!(title.contains("┌ Builder "), "{title}");
    assert!(title.ends_with(" 01:05 ┐"), "{title}");
    // Very narrow: the timer is the last element to disappear.
    let title = draw(&app, 17, 10).lines().next().unwrap().to_string();
    assert!(title.contains("┌ Builder"), "{title}");
    assert!(!title.contains("01:05"), "{title}");
}

#[test]
fn the_timer_hides_while_idle_and_returns_for_the_next_session() {
    // Idle: no timer on the title line, and the rail's first tile sits below
    // its blank top row (T63.1) beside the output frame's first content row.
    let mut app = app();
    let title = draw(&app, 80, 14).lines().next().unwrap().to_string();
    assert!(title.trim_start().starts_with("┌ Builder"), "{title}");
    assert!(
        draw(&app, 80, 14)
            .lines()
            .nth(1)
            .unwrap()
            .starts_with("[ Research ]"),
        "the rail's first tile sits one row below the top"
    );
    assert!(!title.contains("00:00"), "{title}");

    // A session starts and the timer appears.
    let t0 = Instant::now();
    app.now.set(t0);
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    let title = draw(&app, 80, 14).lines().next().unwrap().to_string();
    assert!(title.ends_with(" 00:00 ┐"), "{title}");

    // The session ends: the timer hides again, the left side stays.
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Startup,
    });
    assert_eq!(app.session_elapsed(), None);
    let title = draw(&app, 80, 14).lines().next().unwrap().to_string();
    assert!(title.trim_start().starts_with("┌ Builder"), "{title}");
    assert!(!title.contains("00:00"), "{title}");

    // The next session restarts it from zero.
    app.now.set(t0 + Duration::from_secs(3 * 60 + 7));
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    assert_eq!(app.session_elapsed(), Some(Duration::from_secs(0)));
    let title = draw(&app, 80, 14).lines().next().unwrap().to_string();
    assert!(title.ends_with(" 00:00 ┐"), "{title}");
    app.now.set(app.now.get() + Duration::from_secs(3 * 60 + 7));
    assert_eq!(app.session_elapsed(), Some(Duration::from_secs(3 * 60 + 7)));
    let title = draw(&app, 80, 14).lines().next().unwrap().to_string();
    assert!(title.ends_with(" 03:07 ┐"), "{title}");
}

#[test]
fn output_frame_title_shows_provider_and_model() {
    let mut app = app();
    app.provider = "claude".into();
    app.model = "claude-sonnet-5.5".into();
    insta::assert_snapshot!(draw(&app, 80, 14));
}

#[test]
fn output_frame_title_truncates_when_the_pane_is_narrow() {
    let mut app = app();
    app.provider = "claude".into();
    app.model = "claude-sonnet-5.5".into();
    // 40 columns leave the output frame too narrow for the model.
    insta::assert_snapshot!(draw(&app, 40, 12));
    // Narrower still: the provider goes too, leaving the agent type.
    app.provider = "mistral".into();
    app.model = String::new();
    insta::assert_snapshot!(draw(&app, 26, 10));
}

/// One completed agent session as the shell sees it: a build that ran, streamed
/// one line and ended, leaving the finished line as the pane's last status line.
fn finished_run(agent_name: &str, outcome: SessionOutcome, duration_ms: u64) -> App {
    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::TaskStarted {
        id: "T1.2".into(),
        description: "add the parser for task files".into(),
    });
    app.apply(EngineEvent::AgentChanged {
        agent: agent_name.into(),
        started_ms: 0,
    });
    app.apply(agent(AgentEvent::Text {
        text: "working on it".into(),
    }));
    app.apply(EngineEvent::AgentFinished {
        agent: agent_name.into(),
        outcome,
        duration_ms,
    });
    app.apply(EngineEvent::TaskFinished {
        id: "T1.2".into(),
        outcome: TaskOutcome::Done,
        commit: Some("abc1234".into()),
    });
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Startup,
    });
    app
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
        started_line("planner", "mistral", Some("mistral-large")),
        "Planner started (mistral, mistral-large)"
    );
    assert_eq!(
        started_line("discovery", "claude", None),
        "Discovery started (claude)"
    );
}

#[test]
fn a_started_builder_session_leaves_the_starting_line_before_the_finished_line() {
    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::TaskStarted {
        id: "T1.2".into(),
        description: "add the parser for task files".into(),
    });
    app.apply(EngineEvent::AgentStarted {
        agent: "builder".into(),
        provider: "mock".into(),
        model: None,
    });
    app.apply(EngineEvent::AgentChanged {
        agent: "builder".into(),
        started_ms: 0,
    });
    app.apply(agent(AgentEvent::Text {
        text: "working on it".into(),
    }));
    app.apply(EngineEvent::AgentFinished {
        agent: "builder".into(),
        outcome: SessionOutcome::Finished,
        duration_ms: 125_000,
    });
    // The starting line pairs with the finished line: the pane shows it first
    // (T78.1), exactly once, with no model because none is configured.
    let lines: Vec<&str> = app
        .output
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect();
    let started_at = lines
        .iter()
        .position(|line| *line == "Builder started (mock)")
        .expect("the starting line is in the pane");
    let finished_at = lines
        .iter()
        .position(|line| *line == "Builder finished in 2 min 05 sec")
        .expect("the finished line is in the pane");
    assert!(started_at < finished_at, "lines: {lines:?}");
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("started ("))
            .count(),
        1,
        "lines: {lines:?}"
    );
    // Both lines of the pair are visible at once on a tall terminal, in order.
    let screen = draw(&app, 80, 24);
    assert!(screen.contains("Builder started (mock)"), "{screen}");
    assert!(
        screen.contains("Builder finished in 2 min 05 sec"),
        "{screen}"
    );
    assert!(
        screen.find("Builder started (mock)").unwrap()
            < screen.find("Builder finished in 2 min 05 sec").unwrap(),
        "{screen}"
    );
    insta::assert_snapshot!(screen);
}

#[test]
fn a_started_line_with_a_model_shows_it() {
    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::AgentStarted {
        agent: "builder".into(),
        provider: "mock".into(),
        model: Some("m-brand".into()),
    });
    let screen = draw(&app, 80, 14);
    assert!(
        screen.contains("Builder started (mock, m-brand)"),
        "{screen}"
    );
    insta::assert_snapshot!(screen);
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
fn a_finished_builder_session_leaves_one_status_line_and_hides_the_timer() {
    let app = finished_run("builder", SessionOutcome::Finished, 125_000);
    // The session is over: the title timer hides as before (T31.1).
    assert_eq!(app.session_elapsed(), None);
    let screen = draw(&app, 80, 14);
    assert!(
        screen.contains("Builder finished in 2 min 05 sec"),
        "{screen}"
    );
    assert!(!screen.contains("00:00"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn a_finished_plan_stage_planner_line_precedes_the_builder_announcement() {
    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::TaskStarted {
        id: "T1.2".into(),
        description: "add the parser for task files".into(),
    });
    app.apply(EngineEvent::AgentChanged {
        agent: "planner".into(),
        started_ms: 0,
    });
    app.apply(agent(AgentEvent::Text {
        text: "drafting the plan".into(),
    }));
    app.apply(EngineEvent::AgentFinished {
        agent: "planner".into(),
        outcome: SessionOutcome::Finished,
        duration_ms: 45_000,
    });
    let planner_screen = draw(&app, 80, 14);
    assert!(
        planner_screen.contains("Planner finished in 45 sec"),
        "{planner_screen}"
    );
    insta::assert_snapshot!(planner_screen);

    // The builder takes over: the finished line stays above the builder's output.
    app.apply(EngineEvent::AgentChanged {
        agent: "builder".into(),
        started_ms: 0,
    });
    app.apply(agent(AgentEvent::Text {
        text: "carrying out the plan".into(),
    }));
    let builder_screen = draw(&app, 80, 14);
    assert!(builder_screen.contains("Builder"), "{builder_screen}");
    assert!(
        builder_screen.contains("Planner finished in 45 sec"),
        "{builder_screen}"
    );
    insta::assert_snapshot!(builder_screen);
}

#[test]
fn a_sub_minute_run_formats_seconds_alone() {
    let app = finished_run("builder", SessionOutcome::Finished, 12_000);
    let screen = draw(&app, 80, 14);
    assert!(screen.contains("Builder finished in 12 sec"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn an_over_an_hour_run_formats_hours_and_minutes() {
    let app = finished_run("builder", SessionOutcome::Finished, 3_900_000);
    let screen = draw(&app, 80, 14);
    assert!(
        screen.contains("Builder finished in 1 h 05 min"),
        "{screen}"
    );
    insta::assert_snapshot!(screen);
}

#[test]
fn a_failed_session_still_shows_its_duration() {
    let app = finished_run("builder", SessionOutcome::Failed, 125_000);
    let screen = draw(&app, 80, 14);
    assert!(
        screen.contains("Builder failed in 2 min 05 sec"),
        "{screen}"
    );
    insta::assert_snapshot!(screen);
}

#[test]
fn a_discovery_run_leaves_its_own_finished_line() {
    let mut app = app();
    app.apply(EngineEvent::DiscoveryChanged { discovering: true });
    app.apply(EngineEvent::AgentChanged {
        agent: "discovery".into(),
        started_ms: 0,
    });
    app.apply(agent(AgentEvent::Text {
        text: "scanning the project".into(),
    }));
    app.apply(EngineEvent::AgentFinished {
        agent: "discovery".into(),
        outcome: SessionOutcome::Finished,
        duration_ms: 45_000,
    });
    app.apply(EngineEvent::DiscoveryChanged { discovering: false });
    let screen = draw(&app, 80, 14);
    assert!(screen.contains("Discovery finished in 45 sec"), "{screen}");
    insta::assert_snapshot!(screen);
}

#[test]
fn the_lifecycle_status_lines_carry_the_status_colour_in_full() {
    use patok_core::config::{THEME_KEYS, Theme as ThemeKey};
    use patok_tui::Theme;
    use ratatui::style::Color;

    fn cell_of(buffer: &ratatui::buffer::Buffer, text: &str) -> (u16, u16) {
        (0..24)
            .find_map(|y| {
                let row: String = (0..80).map(|x| buffer[(x, y)].symbol()).collect();
                // A byte index would run past the line on rows that carry
                // multibyte symbols (the frame border), so the match's column
                // is the character count before it.
                row.find(text)
                    .map(|at| (y, row[..at].chars().count() as u16))
            })
            .unwrap_or_else(|| panic!("the line {text:?} is on screen"))
    }

    /// Asserts every cell of the line spelling `text` carries the theme's
    /// pane-status colour, the agent's name inside it included (T117.1).
    fn assert_whole_line(buffer: &ratatui::buffer::Buffer, theme: Theme, text: &str, key: &str) {
        let status = theme.agent_text.pane_status;
        let (row, x) = cell_of(buffer, text);
        for (i, _) in text.chars().enumerate() {
            assert_eq!(
                buffer[(x + i as u16, row)].style().fg,
                Some(status),
                "cell ({}, {}) of {text:?} on {key:?}",
                x + i as u16,
                row
            );
        }
    }

    // The whole lifecycle line wears the status colour on every built-in
    // theme; the fixated agent-name colour reaches only the frame title.
    for name in THEME_KEYS {
        let key = ThemeKey::parse(name).expect("THEME_KEYS holds valid names");
        let theme = Theme::resolve(key, Some(true));
        // The lifecycle status colour matches the task/heading colour (T96.1).
        let status = theme.agent_text.pane_status;
        assert_ne!(status, Color::Reset);
        assert_eq!(status, theme.agent_text.heading);

        let mut finished_app = finished_run("builder", SessionOutcome::Finished, 125_000);
        finished_app.tui.theme = key;
        finished_app.tui.truecolor = Some(true);
        let buffer = draw_buffer(&finished_app);
        assert_whole_line(&buffer, theme, "Builder finished in 2 min 05 sec", name);

        let mut app = app();
        app.tui.theme = key;
        app.tui.truecolor = Some(true);
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        app.apply(EngineEvent::TaskStarted {
            id: "T1.2".into(),
            description: "add the parser for task files".into(),
        });
        app.apply(EngineEvent::AgentStarted {
            agent: "builder".into(),
            provider: "mock".into(),
            model: Some("m-brand".into()),
        });
        app.apply(EngineEvent::AgentChanged {
            agent: "builder".into(),
            started_ms: 0,
        });
        let buffer = draw_buffer(&app);
        assert_whole_line(&buffer, theme, "Builder started (mock, m-brand)", name);
    }
}

#[test]
fn a_reattaching_shell_shows_the_finished_line_from_the_replayed_events() {
    let snapshot = Snapshot {
        project_dir: "/p".into(),
        phase: Phase::Startup,
        tasks: task::parse(TASKS),
        current_task: None,
        planning: false,
        discovering: false,
        provider: String::new(),
        model: String::new(),
        settings: BTreeMap::new(),
        pipeline: PipelineState::today(),
        recent: vec![
            EngineEvent::PhaseChanged {
                phase: Phase::Running,
            },
            EngineEvent::TaskStarted {
                id: "T1.2".into(),
                description: "add the parser for task files".into(),
            },
            EngineEvent::AgentChanged {
                agent: "builder".into(),
                started_ms: 0,
            },
            agent(AgentEvent::Text {
                text: "replayed line".into(),
            }),
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
            EngineEvent::PhaseChanged {
                phase: Phase::Startup,
            },
        ],
    };
    let app = App::new(snapshot, "0.1.0".into());
    assert!(app.is_idle());
    assert_eq!(app.session_elapsed(), None);
    let screen = draw(&app, 80, 14);
    assert!(
        screen.contains("Builder finished in 2 min 05 sec"),
        "{screen}"
    );
}

#[test]
fn a_reattaching_shell_shows_the_starting_line_from_the_replayed_events() {
    let snapshot = Snapshot {
        project_dir: "/p".into(),
        phase: Phase::Startup,
        tasks: task::parse(TASKS),
        current_task: None,
        planning: false,
        discovering: false,
        provider: String::new(),
        model: String::new(),
        settings: BTreeMap::new(),
        pipeline: PipelineState::today(),
        recent: vec![
            EngineEvent::PhaseChanged {
                phase: Phase::Running,
            },
            EngineEvent::TaskStarted {
                id: "T1.2".into(),
                description: "add the parser for task files".into(),
            },
            EngineEvent::AgentStarted {
                agent: "builder".into(),
                provider: "mock".into(),
                model: Some("m-brand".into()),
            },
            EngineEvent::AgentChanged {
                agent: "builder".into(),
                started_ms: 0,
            },
            agent(AgentEvent::Text {
                text: "replayed line".into(),
            }),
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
            EngineEvent::PhaseChanged {
                phase: Phase::Startup,
            },
        ],
    };
    let app = App::new(snapshot, "0.1.0".into());
    assert!(app.is_idle());
    // The replayed starting line pairs with the finished line, in order (T78.1).
    let lines: Vec<&str> = app
        .output
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect();
    let started_at = lines
        .iter()
        .position(|line| *line == "Builder started (mock, m-brand)")
        .expect("the starting line is replayed into the pane");
    let finished_at = lines
        .iter()
        .position(|line| *line == "Builder finished in 2 min 05 sec")
        .expect("the finished line is replayed into the pane");
    assert!(started_at < finished_at, "lines: {lines:?}");
    let screen = draw(&app, 80, 24);
    assert!(
        screen.contains("Builder started (mock, m-brand)"),
        "{screen}"
    );
    assert!(
        screen.contains("Builder finished in 2 min 05 sec"),
        "{screen}"
    );
    assert!(
        screen.find("Builder started (mock, m-brand)").unwrap()
            < screen.find("Builder finished in 2 min 05 sec").unwrap(),
        "{screen}"
    );
}

#[test]
fn an_agent_changed_with_started_ms_reanchors_the_running_timer() {
    let mut app = app();
    let t0 = Instant::now();
    app.now.set(t0);
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    // The busy transition anchors at arrival until an announcement replaces it.
    app.now.set(t0 + Duration::from_secs(10));
    assert_eq!(app.session_elapsed(), Some(Duration::from_secs(10)));

    // The engine-recorded start (65 seconds ago) re-anchors the timer, so it
    // and the finished line derive from the same session timing (T42.1).
    let epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    app.apply(EngineEvent::AgentChanged {
        agent: "builder".into(),
        started_ms: epoch_ms - 65_000,
    });
    let start = app.session_start.expect("the timer is anchored");
    let elapsed = Instant::now().duration_since(start).as_secs();
    assert!(
        (60..=70).contains(&elapsed),
        "anchored {elapsed} seconds back"
    );
    app.now.set(start + Duration::from_secs(65));
    assert_eq!(app.session_elapsed(), Some(Duration::from_secs(65)));

    // A legacy announcement without a start time leaves the anchor alone.
    let before = app.session_start;
    app.apply(EngineEvent::AgentChanged {
        agent: "builder".into(),
        started_ms: 0,
    });
    assert_eq!(app.session_start, before);
}

#[test]
fn reattach_replays_recent_output_without_counting_it() {
    let snapshot = Snapshot {
        project_dir: "/p".into(),
        phase: Phase::Running,
        tasks: task::parse(TASKS),
        current_task: Some("T1.2".into()),
        planning: false,
        discovering: false,
        provider: String::new(),
        model: String::new(),
        settings: BTreeMap::new(),
        pipeline: PipelineState::today(),
        recent: vec![
            EngineEvent::TaskFinished {
                id: "T1.1".into(),
                outcome: TaskOutcome::Done,
                commit: None,
            },
            EngineEvent::TaskStarted {
                id: "T1.2".into(),
                description: "add the parser for task files".into(),
            },
            agent(AgentEvent::Text {
                text: "replayed line".into(),
            }),
        ],
    };
    let app = App::new(snapshot, "0.1.0".into());
    assert_eq!(app.completed, 0);
    assert!(draw(&app, 80, 13).contains("replayed line"));
}

#[test]
fn keys_map_to_actions() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_tui::Action;
    let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

    let mut app = app();
    // Enter starts the build loop while pending tasks remain (T46.1).
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Action::StartBuild
    );
    // The old `s` key no longer triggers anything.
    assert_eq!(app.on_key(key('s')), Action::None);
    assert!(!app.stop_open);
    assert_eq!(app.status, None);
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    // While running, Esc opens the stop dialog instead (T46.1).
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        Action::None
    );
    assert!(app.stop_open);
    assert_eq!(app.status, None);
    // A second Esc closes it; the build continues.
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        Action::None
    );
    assert!(!app.stop_open);
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Action::Detach
    );
    assert_eq!(app.on_key(key('q')), Action::Quit);
    assert!(app.stopping);
    // Once quitting, nothing but detach, a second q (the NOW interrupt) and
    // Esc (the cancel) does anything.
    assert_eq!(app.on_key(key('q')), Action::Interrupt);
    assert_eq!(app.on_key(key('s')), Action::None);
    assert_eq!(app.on_key(key('d')), Action::Detach);
}

/// The `s` key is gone (T46.1): it triggers no action, no hint and no status
/// message in any engine state.
#[test]
fn the_s_key_triggers_nothing_in_any_state() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_tui::Action;
    let s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);

    // Idle: no build start, no message.
    let mut idle = app();
    assert_eq!(idle.on_key(s), Action::None);
    assert_eq!(idle.status, None);
    assert!(!idle.stop_open);
    assert!(!idle.stopping);

    // While a build runs: no stop dialog, no message.
    let mut running = app();
    running.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    assert_eq!(running.on_key(s), Action::None);
    assert!(!running.stop_open);
    assert_eq!(running.status, None);

    // During a planner run: no refusal, the run's own notice stays.
    let mut planning = app();
    planning.apply(EngineEvent::AgentChanged {
        agent: "planner".into(),
        started_ms: 0,
    });
    planning.apply(EngineEvent::PlanningChanged { planning: true });
    assert_eq!(planning.on_key(s), Action::None);
    assert_eq!(planning.status.as_deref(), Some("Planner running..."));

    // During a discovery round: no refusal, no message.
    let mut discovering = app();
    discovering.apply(EngineEvent::DiscoveryChanged { discovering: true });
    assert_eq!(discovering.on_key(s), Action::None);
    assert_eq!(discovering.status, None);

    // While a soft stop is pending: no escalation to the interrupt.
    let mut stopping = app();
    stopping.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    assert_eq!(
        stopping.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        Action::Quit
    );
    assert_eq!(stopping.on_key(s), Action::None);
    assert!(stopping.stopping, "the pending soft stop is untouched");
    assert!(!stopping.stop_open);
}

/// The m menu opens with `m` from every engine state, so the status bar
/// advertises it beside the idle Enter hint in all of them (T128.1); the
/// settings, theme, detach and quit chips it replaced are gone.
#[test]
fn the_status_bar_advertises_the_menu_key_in_every_engine_state() {
    use ratatui::style::Modifier;

    // A wide terminal fits the merged status line whole (T86.1): the chips on
    // the left, the hint strip right-aligned behind them, so the exact line
    // pins the menu chip's place and the order of the remaining hint.
    let strip = |app: &App| draw(app, 130, 14).lines().last().unwrap().to_string();
    let merged = |chips: &str, hints: &str| {
        let pad = 130 - chips.chars().count() - hints.chars().count() - 1;
        format!("{chips}{}{hints}", " ".repeat(pad))
    };

    let idle = app();
    assert_eq!(
        strip(&idle),
        merged(" STOPPED  sprint ", " Enter  start  m  menu")
    );

    let mut running = app();
    running.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    assert_eq!(strip(&running), merged(" RUNNING  sprint ", " m  menu"));

    let mut planning = app();
    planning.apply(EngineEvent::AgentChanged {
        agent: "planner".into(),
        started_ms: 0,
    });
    planning.apply(EngineEvent::PlanningChanged { planning: true });
    // The transient planner notice covers the strip while it runs (T29.1);
    // clearing it shows the planning state's persistent hints.
    planning.status = None;
    assert_eq!(strip(&planning), merged(" PLANNING  sprint ", " m  menu"));

    let mut discovering = app();
    discovering.apply(EngineEvent::DiscoveryChanged { discovering: true });
    assert_eq!(
        strip(&discovering),
        merged(" DISCOVERING  sprint ", " m  menu")
    );

    // The add-tasks, Tab and scroll chips are gone from the status line
    // (T73.1), and so are the secondary chips the m menu replaced: their keys
    // still work through the menu's entries.
    for app in [&idle, &running, &planning, &discovering] {
        let strip = strip(app);
        assert!(!strip.contains(" add tasks "), "{strip}");
        assert!(!strip.contains(" scroll "), "{strip}");
        assert!(!strip.contains(" Tab "), "{strip}");
        assert!(!strip.contains(" settings "), "{strip}");
        assert!(!strip.contains(" theme "), "{strip}");
        assert!(!strip.contains(" detach "), "{strip}");
        assert!(!strip.contains(" quit "), "{strip}");
        assert!(!strip.contains(" stop build "), "{strip}");
    }

    // The shorter strip now fits an 80-column line whole, through the menu
    // chip; no `s` chip survives anywhere.
    let narrow = draw(&idle, 80, 14).lines().last().unwrap().to_string();
    assert!(narrow.contains(" menu"), "{narrow}");
    assert!(narrow.ends_with("menu"), "{narrow}");
    assert!(!narrow.contains(" s "), "{narrow}");

    // The menu chip wears the same status-bar colours as the Enter hint
    // beside it, so it recolours with the active theme (T34.1).
    let theme = patok_tui::Theme::DARK;
    let mut terminal = Terminal::new(TestBackend::new(130, 14)).unwrap();
    terminal.draw(|frame| render(frame, &idle)).unwrap();
    let buffer = terminal.backend().buffer();
    let row: u16 = 13;
    // One symbol per column, so chip positions stay column indices -- the
    // strip's arrows are multi-byte, which would skew a plain `str::find`.
    let symbols: Vec<&str> = (0..130u16).map(|x| buffer[(x, row)].symbol()).collect();
    let column = |chip: &[&str]| {
        (0..=130 - chip.len())
            .find(|&x| (0..chip.len()).all(|i| symbols[x + i] == chip[i]))
            .unwrap_or_else(|| panic!("{chip:?} is not on the strip"))
    };
    let enter = column(&[" ", "E", "n", "t", "e", "r", " "]);
    let menu_key = column(&[" ", "m", " "]);
    for offset in 0..3u16 {
        assert_eq!(
            buffer[(menu_key as u16 + offset, row)].style(),
            buffer[(enter as u16 + offset, row)].style(),
            "the menu chip must wear the Enter chip's colours"
        );
    }
    let chip = buffer[(menu_key as u16, row)].style();
    assert_eq!(chip.fg, Some(theme.contrast_text));
    assert_eq!(chip.bg, Some(theme.chip_neutral));
    assert!(chip.add_modifier.contains(Modifier::BOLD));
    for offset in 0..6u16 {
        assert_eq!(
            buffer[(menu_key as u16 + 3 + offset, row)].style(),
            buffer[(enter as u16 + 7 + offset, row)].style(),
            "the menu label must wear the Enter label's colours"
        );
    }
    assert_eq!(
        buffer[(menu_key as u16 + 3, row)].style().fg,
        Some(theme.muted_text)
    );
    // The menu label is the strip's last content.
    assert_eq!(symbols[menu_key + 3..menu_key + 9].concat(), " menu ");
}

/// The status bar's key hints are chips, not modal buttons (T46.1): in both
/// the default and a palette theme every chip wears the chip colours and
/// every label the muted status colour -- no status-bar cell ever carries the
/// modal button accent (T64.1).
#[test]
fn the_status_bar_hints_never_wear_the_button_accent() {
    use patok_core::config::Theme as ThemeKey;

    for theme_key in [ThemeKey::Dark, ThemeKey::TokyoNightDark] {
        let mut app = app();
        app.tui.theme = theme_key;
        app.tui.truecolor = Some(true);
        let theme = patok_tui::Theme::resolve(theme_key, Some(true));
        let mut terminal = Terminal::new(TestBackend::new(130, 14)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let row: u16 = 13;
        // One symbol per column, so chip positions stay column indices.
        let symbols: Vec<&str> = (0..130u16).map(|x| buffer[(x, row)].symbol()).collect();
        let column = |chip: &[&str]| {
            (0..=130 - chip.len())
                .find(|&x| (0..chip.len()).all(|i| symbols[x + i] == chip[i]))
                .unwrap_or_else(|| panic!("{chip:?} is not on the strip"))
        };
        let menu_key = column(&[" ", "m", " "]);
        for offset in 0..3u16 {
            let style = buffer[(menu_key as u16 + offset, row)].style();
            assert_eq!(style.fg, Some(theme.contrast_text), "the chip text colour");
            assert_eq!(style.bg, Some(theme.chip_neutral), "the chip background");
        }
        for offset in 0..6u16 {
            assert_eq!(
                buffer[(menu_key as u16 + 3 + offset, row)].style().fg,
                Some(theme.muted_text),
                "the label colour"
            );
        }
        assert_eq!(symbols[menu_key + 3..menu_key + 9].concat(), " menu ");
        for x in 0..130u16 {
            assert_ne!(
                buffer[(x, row)].style().fg,
                Some(theme.highlighted_text),
                "no status-bar cell wears the button accent at x={x}"
            );
        }
    }
}

#[test]
fn second_q_interrupts_the_soft_stop() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_tui::Action;
    let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });

    assert_eq!(app.on_key(key('q')), Action::Quit);
    assert!(app.stopping);
    let status = app.status.as_deref().unwrap();
    assert!(status.contains("stops after the current task"));
    assert!(status.contains("press Esc"));
    assert!(status.contains("q to cancel the current task"));
    assert!(status.contains("[d] detach instead"));

    assert_eq!(app.on_key(key('q')), Action::Interrupt);
    // Every other key stays swallowed while stopping.
    assert_eq!(app.on_key(key('a')), Action::None);
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Action::None
    );
    assert_eq!(app.on_key(key('d')), Action::Detach);
}

#[test]
fn esc_cancels_a_pending_soft_stop() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_proto::{ShutdownUpdate, shutdown_request, shutdown_update};
    use patok_tui::Action;
    let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });

    assert_eq!(app.on_key(key('q')), Action::Quit);
    assert!(app.stopping);
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        Action::CancelSoftStop
    );
    assert!(!app.stopping);
    let status = app.status.as_deref().unwrap();
    assert!(status.contains("Soft stop cancelled"), "status: {status}");

    // The pending SOFT shutdown stream ends with the cancelled update, and the
    // session finishes: the shell is back to normal operation.
    let update = ShutdownUpdate {
        phase: shutdown_update::Phase::Complete as i32,
        message: "Soft stop cancelled -- the build keeps running.".into(),
    };
    assert!(
        patok_tui::shutdown_progress(&mut app, shutdown_request::Scope::Soft, &update).is_none()
    );
    assert!(!app.stopping);
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Startup,
    });

    // The keys behave normally again: Enter starts the build (pending tasks
    // remain), `s` does nothing, a opens the dialog.
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Action::StartBuild
    );
    assert_eq!(app.on_key(key('s')), Action::None);
    assert_eq!(app.on_key(key('a')), Action::None);
    assert!(app.dialog_open);
}

#[test]
fn s_during_a_pending_soft_stop_does_nothing() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_tui::Action;
    let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });

    assert_eq!(app.on_key(key('q')), Action::Quit);
    assert!(app.stopping);
    // The old `s` escalation is gone (T46.1): the second q is the only way
    // to interrupt now, so `s` leaves the pending stop untouched.
    assert_eq!(app.on_key(key('s')), Action::None);
    assert!(!app.stop_open, "the stop dialog must not open");
    assert!(app.stopping);
}

#[test]
fn soft_stop_pending_status() {
    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::TaskStarted {
        id: "T1.2".into(),
        description: "add the parser for task files".into(),
    });
    let key = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('q'),
        crossterm::event::KeyModifiers::NONE,
    );
    assert_eq!(app.on_key(key), patok_tui::Action::Quit);
    insta::assert_snapshot!(draw(&app, 80, 14));
}

#[test]
fn version_check_flags_only_differing_builds() {
    let mut app = app();
    let engine = app.engine_version.clone();
    app.check_version(&engine);
    assert!(!app.version_mismatch);
    app.check_version("0.0.0-other");
    assert!(app.version_mismatch);
}

#[test]
fn version_mismatch_indicator() {
    let mut app = app();
    app.check_version("0.2.0");
    let screen = draw(&app, 80, 14);
    assert!(
        screen
            .lines()
            .last()
            .unwrap()
            .starts_with(" STOPPED  sprint"),
        "{screen}"
    );
    assert!(!screen.contains("restart engine"));
    assert!(!screen.contains("≠"));
}

#[test]
fn matching_versions_show_no_indicator() {
    let mut app = app();
    let engine = app.engine_version.clone();
    app.check_version(&engine);
    assert!(!draw(&app, 80, 14).contains("restart engine"));
}

#[test]
fn planning_chip_and_task_list_update_keep_running_task() {
    let mut app = app();
    app.apply(EngineEvent::PlanningChanged { planning: true });
    assert!(
        draw(&app, 60, 12)
            .lines()
            .last()
            .unwrap()
            .starts_with(" PLANNING  sprint")
    );
    app.current_task = Some("T1.2".into());
    app.apply(EngineEvent::TasksChanged {
        tasks: task::parse(&format!("{TASKS}- [ ] T2.1: planned\n")),
    });
    assert_eq!(app.tasks.len(), 4);
    assert_eq!(app.current_task.as_deref(), Some("T1.2"));
    app.apply(EngineEvent::PlanningChanged { planning: false });
    assert!(
        draw(&app, 60, 12)
            .lines()
            .last()
            .unwrap()
            .starts_with(" STOPPED  sprint")
    );
}

#[test]
fn discovery_chip_shows_while_a_round_runs() {
    let mut app = app();
    app.apply(EngineEvent::DiscoveryChanged { discovering: true });
    assert!(
        draw(&app, 60, 12)
            .lines()
            .last()
            .unwrap()
            .starts_with(" DISCOVERING  sprint")
    );
    app.apply(agent(AgentEvent::Text {
        text: "Scanning the project for follow-up work.".into(),
    }));
    insta::assert_snapshot!(draw(&app, 80, 14));
    app.apply(EngineEvent::DiscoveryChanged { discovering: false });
    assert!(
        draw(&app, 60, 12)
            .lines()
            .last()
            .unwrap()
            .starts_with(" STOPPED  sprint")
    );
}

#[test]
fn discovery_blocks_keys_and_its_tasks_reuse_the_new_task_highlight() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_tui::Action;
    let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

    let mut app = app();
    app.apply(EngineEvent::DiscoveryChanged { discovering: true });
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Action::None
    );
    assert_eq!(
        app.status.as_deref(),
        Some("A discovery round is running; wait for it to finish.")
    );
    // `a` shows the same refusal, but only from the focused task list frame
    // (T74.1); the discovery run moved the focus to the agent output frame,
    // so Tab puts it back on the task list first.
    app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.on_key(key('a')), Action::None);
    assert!(!app.dialog_open);
    assert_eq!(
        app.status.as_deref(),
        Some("A discovery round is running; wait for it to finish.")
    );

    // The round's task lands in the list with the same highlight a planner-added task gets.
    app.apply(EngineEvent::TasksChanged {
        tasks: task::parse(&format!("{TASKS}- [ ] D1.1: found by discovery\n")),
    });
    assert!(app.is_new_task("D1.1"));
    assert_eq!(app.tasks.len(), 4);
    // The next key press clears the highlight as with planner-added tasks.
    app.on_key(key('j'));
    assert!(!app.is_new_task("D1.1"));
}

/// Markdown-formatted thinking, the same text both recorded provider fixtures carry.
const THINKING_MD: &str = "## Plan\nRead the **task parser**, then:\n- parse the *task lines*\n- fix the `task.rs` fixture\n- run cargo test\nFinally:\n```rust\nlet tasks = parse(&text);\n```\n";

#[test]
fn thinking_renders_as_markdown() {
    let mut app = app();
    app.apply(EngineEvent::PhaseChanged {
        phase: Phase::Running,
    });
    app.apply(EngineEvent::TaskStarted {
        id: "T1.2".into(),
        description: "add the parser for task files".into(),
    });
    app.apply(agent(AgentEvent::Thinking {
        text: THINKING_MD.into(),
    }));
    // A 24-row screen: the focused output frame has room for the whole thinking
    // block (a 14-row one clips it to its last lines).
    let screen = draw(&app, 80, 24);
    // The markdown syntax itself is gone; only the rendered structure is left.
    assert!(!screen.contains("## Plan"));
    assert!(!screen.contains("**task parser**"));
    assert!(!screen.contains("```"));
    assert!(screen.contains("Plan"));
    assert!(screen.contains("- run cargo test"));
    assert!(screen.contains("let tasks = parse(&text);"));
    insta::assert_snapshot!(screen);
}

#[test]
fn thinking_renders_the_same_regardless_of_provider() {
    // Both providers normalise their recorded thinking streams into the same events,
    // so the rendered screen cannot depend on the provider.
    let mut claude = patok_providers::claude::StreamParser::default();
    let claude_events: Vec<AgentEvent> =
        include_str!("../../patok-providers/tests/fixtures/claude-thinking.jsonl")
            .lines()
            .flat_map(|line| claude.parse(line).events)
            .collect();
    let mut vibe = patok_providers::vibe::StreamParser;
    let vibe_events: Vec<AgentEvent> =
        include_str!("../../patok-providers/tests/fixtures/vibe-thinking.jsonl")
            .lines()
            .flat_map(|line| vibe.parse(line).events)
            .collect();

    let thinking = |events: &[AgentEvent]| -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Thinking { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(
        thinking(&claude_events),
        thinking(&vibe_events),
        "both providers must normalise to the same thinking event"
    );

    let screen = |events: Vec<AgentEvent>| {
        let mut app = app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        for event in events {
            app.apply(agent(event));
        }
        draw(&app, 80, 14)
    };
    let claude_screen = screen(
        thinking(&claude_events)
            .into_iter()
            .map(|text| AgentEvent::Thinking { text })
            .collect(),
    );
    let vibe_screen = screen(
        thinking(&vibe_events)
            .into_iter()
            .map(|text| AgentEvent::Thinking { text })
            .collect(),
    );
    assert_eq!(claude_screen, vibe_screen);
    insta::assert_snapshot!(claude_screen);
}

mod dialog {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_core::event::NoticeLevel;
    use patok_tui::{Action, DialogKind};

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(app: &mut App, c: char) -> Action {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    /// Shift-Enter: the line-break key of the multi-line input (T41.1).
    fn shift_enter(app: &mut App) -> Action {
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT))
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn open() -> App {
        let mut app = app();
        press(&mut app, KeyCode::Char('a'));
        assert!(app.dialog_open);
        app
    }

    fn planner_running() -> App {
        let mut app = open();
        type_text(&mut app, "add a login page");
        assert_eq!(
            ctrl(&mut app, 's'),
            Action::SubmitTasks("add a login page".into())
        );
        app.on_tasks_submitted();
        // The engine announces the run's agent before the planning flag flips (T69.1).
        app.apply(EngineEvent::AgentChanged {
            agent: "planner".into(),
            started_ms: 0,
        });
        app.apply(EngineEvent::PlanningChanged { planning: true });
        app.apply(agent(AgentEvent::Text {
            text: "Reading TASKS.md to see what exists.".into(),
        }));
        app.apply(agent(AgentEvent::ToolUse {
            name: "Edit".into(),
            input: "TASKS.md".into(),
        }));
        app
    }

    #[test]
    fn dialog_is_large_centered_and_inside_the_screen() {
        for (w, h) in [(200u16, 60u16), (50, 12)] {
            let screen = ratatui::layout::Rect::new(0, 0, w, h);
            let rect = patok_tui::dialog_area(screen);
            assert!(screen.contains(ratatui::layout::Position::new(rect.x, rect.y)));
            assert!(
                rect.right() <= w && rect.bottom() <= h,
                "{rect:?} in {w}x{h}"
            );
            if w >= 200 {
                assert!(rect.width > 140 && rect.height > 14 && rect.height >= 48);
            } else {
                assert_eq!((rect.width, rect.height), (w, h));
            }
            assert!(rect.width >= 60.min(w) && rect.height >= 20.min(h));
            assert!(rect.x.abs_diff(w - rect.right()) <= 1);
            assert!(rect.y.abs_diff(h - rect.bottom()) <= 1);
            // Rendering into the same size must not panic.
            draw(&open(), w, h);
        }
    }

    /// T74.1: `a` is the task list frame's key -- it opens the dialog only
    /// while that frame has the focus, and the agent output frame ignores it.
    #[test]
    fn a_opens_the_dialog_only_from_the_focused_task_list_frame() {
        // Idle: the task list frame has the focus, so `a` opens the dialog.
        let mut app = self::app();
        assert_eq!(app.focus, patok_tui::FrameFocus::Tasks);
        assert_eq!(press(&mut app, KeyCode::Char('a')), Action::None);
        assert!(app.dialog_open);

        // A run moves the focus to the agent output frame: the same key is a
        // strict no-op there -- no dialog, no status message.
        let mut app = self::app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        assert_eq!(app.focus, patok_tui::FrameFocus::Output);
        assert_eq!(press(&mut app, KeyCode::Char('a')), Action::None);
        assert!(!app.dialog_open);
        assert!(app.status.is_none());
        // The ignored key does not swallow pane focus toggling: Tab still
        // flips the focus, and from the focused task list frame the busy run
        // refuses the dialog with its inline message.
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, patok_tui::FrameFocus::Tasks);
        press(&mut app, KeyCode::Char('a'));
        assert!(!app.dialog_open);
        assert_eq!(
            app.status.as_deref(),
            Some("A build is running; wait for it to finish.")
        );

        // End to end from the focused task list frame: typing, validation,
        // and the submit flow are unchanged behind the new gate.
        let mut app = self::app();
        assert_eq!(app.focus, patok_tui::FrameFocus::Tasks);
        press(&mut app, KeyCode::Char('a'));
        assert!(app.dialog_open);
        type_text(&mut app, "add a login page");
        assert_eq!(
            ctrl(&mut app, 's'),
            Action::SubmitTasks("add a login page".into())
        );
        app.on_tasks_submitted();
        assert!(!app.dialog_open);
        assert_eq!(app.focus, patok_tui::FrameFocus::Output);
    }

    /// T76.1: `i` is the task list frame's inject key -- it opens the
    /// inject-task modal only while that frame has the focus, from any engine
    /// state (no busy refusal), and the agent output frame ignores it.
    #[test]
    fn i_opens_the_inject_modal_only_from_the_focused_task_list_frame() {
        // Idle: the task list frame has the focus, so `i` opens the inject
        // modal.
        let mut app = self::app();
        assert_eq!(app.focus, patok_tui::FrameFocus::Tasks);
        assert_eq!(press(&mut app, KeyCode::Char('i')), Action::None);
        assert!(app.dialog_open);
        assert_eq!(app.dialog_kind, DialogKind::Inject);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(!app.dialog_open);

        // A run moves the focus to the agent output frame: the same key is a
        // strict no-op there -- no modal, no status message.
        let mut app = self::app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        assert_eq!(app.focus, patok_tui::FrameFocus::Output);
        assert_eq!(press(&mut app, KeyCode::Char('i')), Action::None);
        assert!(!app.dialog_open);
        assert!(app.status.is_none());
        // Tab back to the task list frame: `i` opens the inject modal even
        // while the engine is busy -- a plain file append is safe in every
        // engine state, unlike `a`'s dialog, which a busy run refuses.
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, patok_tui::FrameFocus::Tasks);
        assert_eq!(press(&mut app, KeyCode::Char('i')), Action::None);
        assert!(app.dialog_open);
        assert_eq!(app.dialog_kind, DialogKind::Inject);
        assert!(app.status.is_none());
    }

    /// T76.1: confirming the inject modal returns the typed text as an
    /// `InjectTask` action -- a plain file append by the driver -- so no
    /// agent session begins: the returned action is the only non-`None`
    /// effect and is none of the session-starting variants (`SubmitTasks`,
    /// `ResearchQueue`, `StartBuild`, `RunDiscovery`).
    #[test]
    fn confirming_the_inject_modal_appends_the_typed_line_with_no_agent_run() {
        // Ctrl+S submits through the same path as the Inject button.
        let mut app = self::app();
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "- [ ] T78.1: Polish the README");
        assert_eq!(
            ctrl(&mut app, 's'),
            Action::InjectTask("- [ ] T78.1: Polish the README".into())
        );
        // The driver confirmed the append: the modal closes, the input
        // clears, and the task list frame keeps the focus (contrast with the
        // add flow's move to the output frame).
        app.on_task_injected();
        assert!(!app.dialog_open);
        assert_eq!(app.dialog_text, "");
        assert_eq!(app.focus, patok_tui::FrameFocus::Tasks);

        // Enter confirms through the same path.
        let mut app = self::app();
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "- [ ] T78.1: Polish the README");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::InjectTask("- [ ] T78.1: Polish the README".into())
        );

        // A failed append keeps the modal open on the input with the error.
        let mut app = self::app();
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "- [ ] T78.1: Polish the README");
        assert_eq!(
            ctrl(&mut app, 's'),
            Action::InjectTask("- [ ] T78.1: Polish the README".into())
        );
        app.on_submit_failed("could not append to TASKS.md: nope".into());
        assert!(app.dialog_open);
        assert_eq!(
            app.dialog_status.as_deref(),
            Some("could not append to TASKS.md: nope")
        );
    }

    /// T76.1: the inject modal's validation and cancel behaviour, and the
    /// add-task dialog behind it all is unchanged.
    #[test]
    fn inject_modal_validation_and_cancel() {
        // An empty input is refused inline with the inject modal's own
        // message, by Enter and by the Ctrl+S path alike.
        let mut app = self::app();
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert!(app.dialog_open);
        assert_eq!(
            app.dialog_status.as_deref(),
            Some("Nothing to inject -- type a task line first.")
        );
        assert_eq!(ctrl(&mut app, 's'), Action::None);
        assert!(app.dialog_status.is_some());
        // Esc cancels: the modal closes with no effect.
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(!app.dialog_open);

        // The add-task dialog keeps its own flow: `a` pins the kind back to
        // Add, its empty-input refusal keeps its own message, and its submit
        // still sends the text to the engine and focuses the output frame.
        let mut app = self::app();
        press(&mut app, KeyCode::Char('a'));
        assert!(app.dialog_open);
        assert_eq!(app.dialog_kind, DialogKind::Add);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(
            app.dialog_status.as_deref(),
            Some("Nothing to submit -- type a request first.")
        );
        type_text(&mut app, "add a login page");
        assert_eq!(
            ctrl(&mut app, 's'),
            Action::SubmitTasks("add a login page".into())
        );
        app.on_tasks_submitted();
        assert!(!app.dialog_open);
        assert_eq!(app.focus, patok_tui::FrameFocus::Output);
    }

    /// T77.1: the inject modal confirms while the loop is running -- no busy
    /// state refuses the key or the confirm -- and the injected task appears
    /// in the list as soon as the engine's task-list change arrives; at the
    /// engine level (the mid-build engine test) it runs as the next task
    /// once the current one finishes.
    #[test]
    fn the_inject_modal_confirms_while_the_loop_is_running() {
        let mut app = self::app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        assert_eq!(app.focus, patok_tui::FrameFocus::Output);
        // The T76.1 pattern: Tab back to the task list frame, press `i`,
        // type, confirm -- all mid-run, with no busy refusal anywhere.
        press(&mut app, KeyCode::Tab);
        assert_eq!(press(&mut app, KeyCode::Char('i')), Action::None);
        assert!(app.dialog_open);
        assert_eq!(app.dialog_kind, DialogKind::Inject);
        assert!(app.status.is_none(), "no busy refusal blocks the modal");
        type_text(&mut app, "- [ ] T79.1: Polish the inject path");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::InjectTask("- [ ] T79.1: Polish the inject path".into())
        );
        // The confirm returns success even while a session is active: the
        // modal closes only once the driver reports the engine's acceptance.
        assert!(app.dialog_open);
        app.on_task_injected();
        assert!(!app.dialog_open);
        assert_eq!(app.focus, patok_tui::FrameFocus::Tasks);
        // The engine's immediate reconcile broadcasts the new line: it
        // appears in the list without waiting for the next poll cycle.
        app.apply(EngineEvent::TasksChanged {
            tasks: task::parse("- [ ] T79.1: Polish the inject path\n"),
        });
        assert!(app.tasks.iter().any(|t| t.id == "T79.1" && !t.done));
    }

    /// T77.1: the inject modal renders above the running loop -- the same
    /// modal as while idle, visible mid-build.
    #[test]
    fn the_inject_modal_renders_while_the_loop_is_running() {
        let mut app = self::app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "- [ ] T79.1: Polish the inject path");
        let screen = draw(&app, 80, 24);
        assert!(screen.contains(" Inject a task "), "{screen}");
        assert!(
            screen.contains("- [ ] T79.1: Polish the inject path"),
            "{screen}"
        );
    }

    #[test]
    fn empty_dialog() {
        insta::assert_snapshot!(draw(&open(), 80, 24));
    }

    #[test]
    fn inject_modal_empty() {
        let mut app = self::app();
        press(&mut app, KeyCode::Char('i'));
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    #[test]
    fn dialog_with_text() {
        let mut app = open();
        type_text(&mut app, "add a login page");
        shift_enter(&mut app);
        type_text(&mut app, "and a logout button");
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    #[test]
    fn inject_modal_with_text() {
        let mut app = self::app();
        press(&mut app, KeyCode::Char('i'));
        type_text(&mut app, "- [ ] T78.1: Polish the README");
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    #[test]
    fn accepted_submit_closes_the_dialog_and_focuses_the_output_frame() {
        let mut app = open();
        type_text(&mut app, "add a login page");
        assert_eq!(
            ctrl(&mut app, 's'),
            Action::SubmitTasks("add a login page".into())
        );
        app.on_tasks_submitted();
        assert!(!app.dialog_open, "the accepted submit closes the modal");
        assert_eq!(app.focus, patok_tui::FrameFocus::Output);
        assert_eq!(app.dialog_text, "");
    }

    /// Enter submits the dialog through the same path as the Submit button
    /// (T41.1): the typed text goes out as an `AddTasks` command.
    #[test]
    fn enter_submits_non_empty_input_through_addtasks() {
        let mut app = open();
        type_text(&mut app, "add a login page");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::SubmitTasks("add a login page".into())
        );
        app.on_tasks_submitted();
        assert!(!app.dialog_open, "the accepted submit closes the modal");
        assert_eq!(app.focus, patok_tui::FrameFocus::Output);
        assert_eq!(app.dialog_text, "");

        // A multi-line input submits every line, not just the cursor's.
        let mut app = open();
        type_text(&mut app, "first line");
        shift_enter(&mut app);
        type_text(&mut app, "second line");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Action::SubmitTasks("first line\nsecond line".into())
        );
    }

    /// Shift-Enter breaks the line at the cursor without submitting (T41.1).
    #[test]
    fn shift_enter_inserts_a_newline_without_submitting() {
        let mut app = open();
        type_text(&mut app, "first line");
        assert!(app.dialog_open);
        assert_eq!(shift_enter(&mut app), Action::None);
        assert_eq!(app.cursor(), 11, "the cursor moved past the newline");
        type_text(&mut app, "second line");
        assert_eq!(app.dialog_text, "first line\nsecond line");
        assert!(app.dialog_open, "the dialog never submitted");
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("first line"), "{screen}");
        assert!(screen.contains("second line"), "{screen}");
    }

    /// Shift-Enter breaks the line at the cursor mid-text, after pasted text
    /// and at the very end, and Backspace joins the halves back over the
    /// newline (T62.1).
    #[test]
    fn shift_enter_mid_text_round_trips_with_backspace() {
        // Mid-text: the line splits at the cursor and the cursor lands at the
        // new line's start, without submitting.
        let mut app = open();
        type_text(&mut app, "abcdef");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.cursor(), 3);
        assert_eq!(shift_enter(&mut app), Action::None);
        assert!(app.dialog_open, "Shift-Enter never submits");
        assert_eq!(app.dialog_text, "abc\ndef");
        assert_eq!(app.cursor(), 4, "the cursor sits at the new line's start");
        // Backspace over the newline joins the halves back to the original.
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.dialog_text, "abcdef");
        assert_eq!(app.cursor(), 3, "the cursor returns to the join point");

        // After pasted text: the paste lands first, Shift-Enter splits at the
        // cursor, and Backspace round-trips the same way.
        let mut app = open();
        app.on_paste("one two");
        press(&mut app, KeyCode::Left);
        assert_eq!(shift_enter(&mut app), Action::None);
        assert_eq!(app.dialog_text, "one tw\no");
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.dialog_text, "one two");
        assert_eq!(app.cursor(), 6);

        // At the very end: the newline appends and typing continues below it.
        let mut app = open();
        type_text(&mut app, "end");
        assert_eq!(shift_enter(&mut app), Action::None);
        assert_eq!(app.dialog_text, "end\n");
        type_text(&mut app, "tail");
        assert_eq!(app.dialog_text, "end\ntail");
        press(&mut app, KeyCode::Home);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.dialog_text, "endtail");
        assert_eq!(app.cursor(), 3);
    }

    /// Enter on an empty input refuses inline; the dialog stays open (T41.1).
    #[test]
    fn enter_on_empty_input_keeps_the_inline_refusal() {
        let mut app = open();
        assert_eq!(app.dialog_text, "");
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert!(app.dialog_open, "the dialog stays open");
        assert_eq!(
            app.dialog_status.as_deref(),
            Some("Nothing to submit -- type a request first.")
        );
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    /// The watermark hint shows only while the input is empty (T41.1): the
    /// first typed character replaces it, and clearing the input brings it
    /// back. It is never part of the typed text.
    #[test]
    fn watermark_shows_only_while_the_input_is_empty() {
        let mut app = open();
        assert!(app.dialog_text.is_empty());
        assert!(
            draw(&app, 80, 24).contains("Press Enter to run Discovery agent"),
            "the empty input shows the watermark"
        );
        // Typing replaces it; the typed text carries no watermark.
        type_text(&mut app, "hi");
        assert_eq!(app.dialog_text, "hi");
        assert!(
            !draw(&app, 80, 24).contains("Press Enter to run Discovery agent"),
            "the watermark is gone once text is typed"
        );
        // Clearing the input brings it back.
        ctrl(&mut app, 'u');
        assert!(app.dialog_text.is_empty());
        assert!(
            draw(&app, 80, 24).contains("Press Enter to run Discovery agent"),
            "the watermark returns on the cleared input"
        );
    }

    #[test]
    fn planner_running_on_the_dashboard() {
        let app = planner_running();
        let screen = draw(&app, 80, 24);
        assert!(screen.contains(" PLANNING"), "{screen}");
        assert!(screen.contains(" Planner "), "{screen}");
        assert!(
            screen.contains("Reading TASKS.md to see what exists."),
            "{screen}"
        );
        assert!(screen.contains("Planner running..."), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn planner_finished_on_the_dashboard() {
        let mut app = planner_running();
        app.apply(EngineEvent::Notice {
            level: NoticeLevel::Info,
            text: "2 tasks added.".into(),
        });
        app.apply(EngineEvent::TasksChanged {
            tasks: task::parse(&format!(
                "{TASKS}## Phase 2\n- [ ] T2.1: login page\n- [ ] T2.2: logout button\n"
            )),
        });
        app.apply(EngineEvent::PlanningChanged { planning: false });
        // The outcome lives only in the output frame; the bottom status area is
        // back to its normal content (T29.1).
        assert_eq!(app.status, None);
        assert!(app.is_new_task("T2.1"), "the new tasks are highlighted");
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("2 tasks added."), "{screen}");
        let bottom = screen.lines().last().unwrap();
        assert!(!bottom.contains(" add tasks "), "{screen}");
        assert!(bottom.contains(" menu"), "{screen}");
        // The add-tasks chip is gone from the strip (T73.1); the shorter
        // strip fits an 80-column line whole, through the menu chip.
        assert!(bottom.ends_with("menu"), "{screen}");
        assert!(!bottom.contains("Added 2 tasks"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn planner_error_on_the_dashboard() {
        let mut app = planner_running();
        app.apply(EngineEvent::Notice {
            level: NoticeLevel::Error,
            text: "planner result rejected: task T1.1 was modified; restored".into(),
        });
        app.apply(EngineEvent::PlanningChanged { planning: false });
        assert_eq!(app.status, None);
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("planner result rejected"), "{screen}");
        let bottom = screen.lines().last().unwrap();
        assert!(bottom.contains(" menu"), "{screen}");
        assert!(!bottom.contains(" add tasks "), "{screen}");
        assert!(!bottom.contains("planner result rejected"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    /// The other bottom status messages keep rendering: the busy refusal during a
    /// run and the quit message after it (T29.1).
    #[test]
    fn other_status_messages_still_render_at_the_bottom() {
        let mut app = planner_running();
        // Enter while the planner runs refuses with a message on the last line.
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        let screen = draw(&app, 80, 24);
        assert!(
            screen
                .lines()
                .last()
                .unwrap()
                .contains("The planner is already running."),
            "{screen}"
        );
        // Once the run ends the bottom line is the key-hint strip again, not
        // the run's outcome.
        app.apply(EngineEvent::Notice {
            level: NoticeLevel::Info,
            text: "1 task added.".into(),
        });
        app.apply(EngineEvent::PlanningChanged { planning: false });
        assert_eq!(app.status, None);
        let screen = draw(&app, 80, 24);
        let bottom = screen.lines().last().unwrap();
        assert!(bottom.contains(" menu"), "{screen}");
        assert!(!bottom.contains(" add tasks "), "{screen}");
        assert!(!bottom.contains("1 task added."), "{screen}");
        // The quit message still lands on the last line (a quit from idle uses
        // the NOW scope: the engine stops and the app exits).
        assert_eq!(press(&mut app, KeyCode::Char('q')), Action::Interrupt);
        let screen = draw(&app, 80, 24);
        assert!(
            screen
                .lines()
                .last()
                .unwrap()
                .contains("Stopping the engine..."),
            "{screen}"
        );
    }

    #[test]
    fn command_error_keeps_the_input() {
        let mut app = open();
        type_text(&mut app, "do it");
        assert!(matches!(ctrl(&mut app, 's'), Action::SubmitTasks(_)));
        app.on_submit_failed("a build is running".into());
        assert_eq!(app.dialog_text, "do it");
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    #[test]
    fn typing_editing_and_submitting() {
        let mut app = open();
        type_text(&mut app, "abc");
        press(&mut app, KeyCode::Backspace);
        shift_enter(&mut app);
        type_text(&mut app, "d");
        assert_eq!(app.dialog_text, "ab\nd");
        // Dialog keys do not leak to the shell: `q` is text, not quit.
        type_text(&mut app, "q");
        assert!(!app.stopping);
        app.on_paste("x\r\ny");
        assert_eq!(app.dialog_text, "ab\ndqx\ny");
        ctrl(&mut app, 'u');
        assert_eq!(app.dialog_text, "");
    }

    fn key_with(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.on_key(KeyEvent::new(code, modifiers));
    }

    #[test]
    fn left_right_home_end_and_editing_at_the_cursor() {
        let mut app = open();
        type_text(&mut app, "héllo");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.cursor(), 3);
        type_text(&mut app, "ü");
        assert_eq!(app.dialog_text, "hélülo");
        for _ in 0..3 {
            press(&mut app, KeyCode::Backspace);
        }
        assert_eq!(app.dialog_text, "hlo");
        press(&mut app, KeyCode::Delete);
        assert_eq!(app.dialog_text, "ho");
        press(&mut app, KeyCode::Home);
        assert_eq!(app.cursor(), 0);
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.dialog_text, "ho");
        app.on_paste("日本\n");
        assert_eq!(app.dialog_text, "日本\nho");
        assert_eq!(app.cursor(), 3);
        press(&mut app, KeyCode::End);
        assert_eq!(app.cursor(), 5);
        press(&mut app, KeyCode::Delete);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.cursor(), 5);
        press(&mut app, KeyCode::Home);
        assert_eq!(app.cursor(), 3);
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::End);
        assert_eq!(app.cursor(), 2);
        shift_enter(&mut app);
        assert_eq!(app.dialog_text, "日本\n\nho");
        ctrl(&mut app, 'u');
        assert_eq!((app.dialog_text.as_str(), app.cursor()), ("", 0));
    }

    #[test]
    fn word_movement() {
        let mut app = open();
        type_text(&mut app, "one  two three");
        key_with(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
        assert_eq!(app.cursor(), 9);
        key_with(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
        assert_eq!(app.cursor(), 5);
        key_with(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
        key_with(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
        assert_eq!(app.cursor(), 0);
        key_with(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(app.cursor(), 3);
        key_with(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(app.cursor(), 8);
    }

    #[test]
    fn up_down_between_lines_keep_the_column() {
        let mut app = open();
        app.on_paste("abcdef\nab\nabcdef");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.cursor(), 14);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.cursor(), 9, "clamped to the short line");
        press(&mut app, KeyCode::Up);
        assert_eq!(app.cursor(), 4, "the column is remembered");
        press(&mut app, KeyCode::Up);
        assert_eq!(app.cursor(), 4, "no row above");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.cursor(), 14);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.cursor(), 14);
    }

    #[test]
    fn up_down_between_wrapped_rows() {
        let mut app = open();
        app.dialog_width.set(5);
        app.on_paste("abcdefghijkl");
        // Rows: abcde / fghij / kl
        assert_eq!(app.cursor(), 12);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.cursor(), 7);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.cursor(), 2);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.cursor(), 12);
    }

    #[test]
    fn long_text_scrolls_to_keep_the_cursor_visible() {
        let mut app = open();
        let text = (1..=30)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.on_paste(&text);
        assert!(draw(&app, 80, 24).contains("line 30"));
        for _ in 0..29 {
            press(&mut app, KeyCode::Up);
        }
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("line 1"));
        assert!(!screen.contains("line 30"));
    }

    #[test]
    fn dialog_with_cursor_mid_text() {
        let mut app = open();
        type_text(&mut app, "add a login page");
        shift_enter(&mut app);
        type_text(&mut app, "and a logout button");
        press(&mut app, KeyCode::Up);
        for _ in 0..4 {
            press(&mut app, KeyCode::Left);
        }
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    #[test]
    fn dialog_with_cursor_at_start() {
        let mut app = open();
        type_text(&mut app, "add a login page");
        press(&mut app, KeyCode::Home);
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    /// The dialog input's first cell on an 80x24 screen.
    fn input_origin() -> (u16, u16) {
        let area = patok_tui::dialog_area(ratatui::layout::Rect::new(0, 0, 80, 24));
        (area.x + 1, area.y + 1)
    }

    #[test]
    fn block_cursor_renders_on_the_right_cell() {
        use patok_tui::Theme;
        use ratatui::style::Color;
        let (x0, y0) = input_origin();
        // The block cursor's fg/bg pair (the cell may carry an underline colour
        // reset the Paragraph renderer adds). The colours come from the theme, so
        // this test follows any future cursor restyling.
        let is_block = |cell: &ratatui::buffer::Cell| {
            cell.style().fg == Some(Theme::DARK.contrast_text)
                && cell.style().bg == Some(Theme::DARK.accent)
        };
        let is_plain = |cell: &ratatui::buffer::Cell| {
            matches!(cell.style().fg, None | Some(Color::Reset))
                && matches!(cell.style().bg, None | Some(Color::Reset))
        };

        // Mid-text: the character under the cursor carries the block style and
        // its neighbours stay plain.
        let mut app = open();
        type_text(&mut app, "hello");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.cursor(), 3);
        let buffer = draw_buffer(&app);
        let under = &buffer[(x0 + 3, y0)];
        assert_eq!(under.symbol(), "l");
        assert!(
            is_block(under),
            "the cursor cell is a block: {:?}",
            under.style()
        );
        for (x, symbol) in [(x0 + 1, "e"), (x0 + 4, "o")] {
            let neighbour = &buffer[(x, y0)];
            assert_eq!(neighbour.symbol(), symbol);
            assert!(
                is_plain(neighbour),
                "the neighbour is plain: {:?}",
                neighbour.style()
            );
        }

        // Empty input: the watermark hint fills the first cells in the dim
        // hint colour, and the block cursor is the styled space after it (T41.1).
        let watermark = patok_tui::DIALOG_WATERMARK;
        let width = watermark.chars().count() as u16;
        let buffer = draw_buffer(&open());
        for (i, ch) in watermark.chars().enumerate() {
            let cell = &buffer[(x0 + i as u16, y0)];
            assert_eq!(cell.symbol(), ch.to_string());
            assert_eq!(
                cell.style().fg,
                Some(Theme::DARK.muted_text),
                "the watermark is dim, not typed content: {:?}",
                cell.style()
            );
        }
        assert_eq!(buffer[(x0 + width, y0)].symbol(), " ");
        assert!(is_block(&buffer[(x0 + width, y0)]));

        // Cursor at the end of the text: a styled space after the last letter.
        let mut app = open();
        type_text(&mut app, "ab");
        let buffer = draw_buffer(&app);
        assert_eq!(buffer[(x0, y0)].symbol(), "a");
        assert_eq!(buffer[(x0 + 1, y0)].symbol(), "b");
        assert_eq!(buffer[(x0 + 2, y0)].symbol(), " ");
        assert!(is_block(&buffer[(x0 + 2, y0)]));

        // A wrapped row: the cursor block sits at the end of the last visual
        // row (rows wrap at 69 columns: 0..69 then 69..100).
        let mut app = open();
        app.on_paste(&"x".repeat(100));
        let buffer = draw_buffer(&app);
        assert_eq!(buffer[(x0 + 31, y0 + 1)].symbol(), " ");
        assert!(is_block(&buffer[(x0 + 31, y0 + 1)]));
    }

    #[test]
    fn letters_stay_in_place_while_the_cursor_moves() {
        let mut app = open();
        type_text(&mut app, "add a login page");
        let (x0, y0) = input_origin();
        // The first 16 input cells always spell the typed text, whatever the
        // cursor does: the block cursor covers its cell in place instead of
        // inserting a glyph that shifts the letters.
        let check = |app: &App| {
            let buffer = draw_buffer(app);
            let symbols: String = (0..16).map(|i| buffer[(x0 + i, y0)].symbol()).collect();
            assert_eq!(symbols, "add a login page");
            assert!(
                !draw(app, 80, 24).contains('▏'),
                "the thin cursor glyph is gone"
            );
        };
        check(&app);
        for _ in 0..16 {
            press(&mut app, KeyCode::Left);
            check(&app);
        }
        for _ in 0..4 {
            key_with(&mut app, KeyCode::Left, KeyModifiers::CONTROL);
            check(&app);
        }
        for _ in 0..4 {
            key_with(&mut app, KeyCode::Right, KeyModifiers::CONTROL);
            check(&app);
        }
        press(&mut app, KeyCode::Home);
        check(&app);
        press(&mut app, KeyCode::End);
        check(&app);
    }

    #[test]
    fn escape_closes_and_keeps_the_text() {
        let mut app = open();
        type_text(&mut app, "keep me");
        press(&mut app, KeyCode::Esc);
        assert!(!app.dialog_open);
        press(&mut app, KeyCode::Char('a'));
        assert!(app.dialog_open);
        assert_eq!(app.dialog_text, "keep me");
    }

    #[test]
    fn ctrl_c_detaches_from_the_dialog() {
        let mut app = open();
        assert_eq!(ctrl(&mut app, 'c'), Action::Detach);
    }

    #[test]
    fn planner_output_renders_thinking_as_markdown() {
        let mut app = planner_running();
        app.apply(agent(AgentEvent::Thinking {
            text: super::THINKING_MD.into(),
        }));
        // The main agent output frame renders the planner's thinking as markdown.
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("Read the"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn keys_and_tab_switches_during_a_planner_run_leave_it_going() {
        let mut app = planner_running();
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(app.focus, patok_tui::FrameFocus::Output);
        assert_eq!(press(&mut app, KeyCode::Tab), Action::None);
        assert_eq!(app.focus, patok_tui::FrameFocus::Tasks);
        assert_eq!(press(&mut app, KeyCode::Tab), Action::None);
        assert_eq!(app.focus, patok_tui::FrameFocus::Output);
        assert!(app.planning, "switching the focus leaves the run going");
        assert!(!app.dialog_open, "the dialog stays closed");
    }

    #[test]
    fn dialog_is_blocked_while_building_or_planning() {
        let mut app = app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        // The key belongs to the task list frame (T74.1), and a run moves the
        // focus to the agent output frame, so it does nothing there.
        press(&mut app, KeyCode::Char('a'));
        assert!(!app.dialog_open);
        assert!(app.status.is_none());
        // From the focused task list frame the busy run refuses the dialog.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('a'));
        assert!(!app.dialog_open);
        assert_eq!(
            app.status.as_deref(),
            Some("A build is running; wait for it to finish.")
        );

        let mut app = self::app();
        app.apply(EngineEvent::PlanningChanged { planning: true });
        // The event's own status line ("planner running...") stays as it is;
        // the key itself adds nothing to it.
        let before = app.status.clone();
        press(&mut app, KeyCode::Char('a'));
        assert!(!app.dialog_open);
        assert_eq!(app.status, before);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('a'));
        assert!(!app.dialog_open);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert_eq!(
            app.status.as_deref(),
            Some("The planner is already running.")
        );
    }

    #[test]
    fn new_tasks_are_highlighted_until_the_next_key() {
        let mut app = app();
        app.apply(EngineEvent::TasksChanged {
            tasks: task::parse(&format!("{TASKS}- [ ] T2.1: planned\n")),
        });
        assert!(app.is_new_task("T2.1"));
        assert!(!app.is_new_task("T1.2"));
        press(&mut app, KeyCode::Char('j'));
        assert!(!app.is_new_task("T2.1"));
    }

    /// The Submit button hint shows the real binding (Enter, T41.1) and no
    /// Ctrl+S wording survives anywhere in the rendered dialog or its status
    /// line (T44.1).
    #[test]
    fn submit_button_hint_shows_enter_and_no_ctrl_s() {
        // Empty input: the primary label is Start.
        let screen = draw(&open(), 80, 24);
        assert!(screen.contains("[ Enter ] Start"), "{screen}");
        assert!(screen.contains("[ Esc ] Close"), "{screen}");
        assert!(!screen.contains("Ctrl+S"), "{screen}");

        // Typed input: the primary label is Submit.
        let mut app = open();
        type_text(&mut app, "add a login page");
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("[ Enter ] Submit"), "{screen}");
        assert!(!screen.contains("Ctrl+S"), "{screen}");

        // The inline refusal keeps the hint and adds no Ctrl+S to the status line.
        let mut app = open();
        press(&mut app, KeyCode::Enter);
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("Nothing to submit"), "{screen}");
        assert!(!screen.contains("Ctrl+S"), "{screen}");

        // The brief-saved status line points at Enter too.
        let mut app = open();
        app.on_brief_saved();
        let status = app.dialog_status.as_deref().unwrap_or_default();
        assert!(status.contains("Enter"), "{status}");
        assert!(!status.contains("Ctrl+S"), "{status}");
    }

    /// The dialog's buttons are the settings overlay's button widget (T44.1):
    /// the same " [ key ] " bracket marker, padding and theme colour -- every
    /// covered cell's style equals the overlay's own [ x ] marker's style.
    #[test]
    fn dialog_buttons_wear_the_settings_overlay_button_style() {
        use patok_tui::Theme;
        use ratatui::style::{Color, Modifier, Style};

        // The styles of every cell covered by `needle` in the buffer.
        let styles = |buffer: &ratatui::buffer::Buffer, needle: &str| -> Vec<Style> {
            for y in 0..buffer.area.height {
                let row: String = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect();
                if let Some(at) = row.find(needle) {
                    // Box-drawing rows are multi-byte, so byte offsets are not
                    // columns: count the characters before the match.
                    let x = row[..at].chars().count() as u16;
                    let len = needle.chars().count() as u16;
                    return (x..x + len).map(|i| buffer[(i, y)].style()).collect();
                }
            }
            panic!("{needle:?} not found in the buffer");
        };

        // The overlay's own close button is the reference style: the theme's
        // close colour, no background, no bold.
        let mut overlay = app();
        press(&mut overlay, KeyCode::Char('?'));
        assert!(overlay.settings_open());
        let close = styles(&draw_buffer(&overlay), "[ x ]");
        assert!(
            close.iter().all(|style| {
                style.fg == Some(Theme::DARK.highlighted_text)
                    && matches!(style.bg, None | Some(Color::Reset))
                    && !style.add_modifier.contains(Modifier::BOLD)
            }),
            "the overlay's [ x ] wears the theme's close colour: {close:?}"
        );
        let reference = close[0];

        // The dialog's buttons wear exactly that style, cell for cell.
        let buffer = draw_buffer(&open());
        for style in styles(&buffer, "[ Enter ]") {
            assert_eq!(style, reference);
        }
        for style in styles(&buffer, "[ Esc ]") {
            assert_eq!(style, reference);
        }
    }

    /// The dialog's bottom line names Shift-Enter as the new-line key (T62.1):
    /// a hint without brackets, in the low-emphasis footer colour -- never the
    /// button accent -- alongside the existing hints (the tail hint gives up
    /// its width first, T59.1) and the buttons on the same row.
    #[test]
    fn dialog_footer_hints_at_shift_enter() {
        use patok_tui::Theme;

        let buffer = draw_buffer(&open());
        let needle = "Shift-Enter new line";
        let mut found = None;
        for y in 0..buffer.area.height {
            let row: String = (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect();
            if let Some(at) = row.find(needle) {
                found = Some((y, row, at));
                break;
            }
        }
        let (y, row, at) = found.expect("the footer names Shift-Enter");
        assert!(!row.contains("[ Shift-Enter"), "a hint wears no brackets");
        assert!(row.contains("[ Enter ]"), "{row}");
        assert!(row.contains("[ Esc ]"), "{row}");
        let x = row[..at].chars().count() as u16;
        for i in x..x + needle.chars().count() as u16 {
            let style = buffer[(i, y)].style();
            assert_eq!(
                style.fg,
                Some(Theme::DARK.muted_text),
                "the hint wears the low-emphasis footer colour"
            );
            assert_ne!(
                style.fg,
                Some(Theme::DARK.highlighted_text),
                "the hint never wears the button accent"
            );
        }
    }
}

/// The startup scenarios and the scenario-driven submit
///.
mod explore {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_core::config::SettingValue;
    use patok_core::event::Snapshot;
    use patok_core::scenario::{ProjectScan, Scenario, SpecState, TaskFileState};
    use patok_core::task;
    use patok_tui::{Action, FrameFocus, QueueRun};

    /// An app whose task queue is `tasks`.
    fn app_with(tasks: &str) -> App {
        App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: task::parse(tasks),
                current_task: None,
                planning: false,
                discovering: false,
                provider: String::new(),
                model: String::new(),
                settings: BTreeMap::new(),
                pipeline: PipelineState::today(),
                recent: vec![],
            },
            "0.1.0".into(),
        )
    }

    /// Project facts for one scenario.
    fn status(has_code: bool, task_file: TaskFileState, spec: SpecState) -> ProjectScan {
        ProjectScan {
            has_code,
            task_file,
            spec,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn open_dialog(app: &mut App) {
        app.on_key(key(KeyCode::Char('a')));
        assert!(app.dialog_open, "the dialog must be open");
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    fn submit(app: &mut App) -> Action {
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
    }

    #[test]
    fn explore_empty_project() {
        let mut app = app_with("");
        app.project_status = status(false, TaskFileState::Missing, SpecState::Missing);
        assert_eq!(app.scenario(), Scenario::EmptyProject);
        let screen = draw(&app, 80, 14);
        assert!(!screen.contains("Start a new project"), "{screen}");
        assert!(!screen.contains("not a git repo"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn explore_needs_queue() {
        let mut app = app_with("");
        app.project_status = status(true, TaskFileState::Missing, SpecState::Content);
        assert_eq!(app.scenario(), Scenario::NeedsQueue);
        let screen = draw(&app, 80, 14);
        assert!(
            !screen.contains("Code found, but no task queue exists yet."),
            "{screen}"
        );
        assert!(
            !screen.contains("SPEC.md found · Task queue: missing"),
            "{screen}"
        );
        assert!(!screen.contains("main · 3 dirty · no remote"), "{screen}");
        assert!(!screen.contains("Last commit: abc1234"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn explore_queue_ready() {
        let mut app = app_with(TASKS);
        app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        assert_eq!(app.scenario(), Scenario::QueueReady);
        let screen = draw(&app, 80, 14);
        assert!(!screen.contains("Work is ready to continue."), "{screen}");
        assert!(!screen.contains("Tasks: 1/3 complete · 2 left"), "{screen}");
        assert!(!screen.contains("main · 1 dirty · origin"), "{screen}");
        assert!(
            !screen.contains("Next: T1.2 add the parser for task files"),
            "{screen}"
        );
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn explore_queue_complete() {
        let mut app = app_with("- [x] T1.1: first done\n- [x] T1.2: second done\n");
        app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        app.status = Some("transient warning".into());
        assert_eq!(app.scenario(), Scenario::QueueComplete);
        let screen = draw(&app, 80, 14);
        assert!(
            !screen.contains("Current task queue is complete."),
            "{screen}"
        );
        // The transient warning keeps its place in the status bar.
        assert!(screen.contains("transient warning"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn the_app_opens_with_the_task_list_focused_and_tab_switches_the_focus() {
        let mut app = app_with(TASKS);
        app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        assert_eq!(app.focus, FrameFocus::Tasks, "idle opens on the task list");
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, FrameFocus::Output);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, FrameFocus::Tasks);
    }

    #[test]
    fn shift_tab_flips_the_run_mode_both_ways() {
        let mut app = app_with(TASKS);
        app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        // The engine-reported readout defaults to sprint, so the first press
        // flips to continuous.
        assert_eq!(app.run_mode(), "sprint");
        assert_eq!(
            app.on_key(key(KeyCode::BackTab)),
            Action::SetRunMode("continuous".into())
        );
        assert_eq!(
            app.status.as_deref(),
            Some("Run mode switched to continuous.")
        );
        // The driver applied the change and the readout moved, so the next
        // press flips back. Kitty-enhanced terminals report S-Tab as Tab plus
        // SHIFT, so both encodings take the same path.
        app.settings
            .insert("run_mode".into(), SettingValue::Str("continuous".into()));
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)),
            Action::SetRunMode("sprint".into())
        );
        assert_eq!(app.status.as_deref(), Some("Run mode switched to sprint."));
        // A plain Tab keeps switching the frame focus only, never the run mode.
        assert_eq!(app.focus, FrameFocus::Tasks);
        assert_eq!(app.on_key(key(KeyCode::Tab)), Action::None);
        assert_eq!(app.focus, FrameFocus::Output);
        assert_eq!(app.on_key(key(KeyCode::Tab)), Action::None);
        assert_eq!(app.focus, FrameFocus::Tasks);
    }

    #[test]
    fn shift_tab_is_swallowed_inside_the_modals() {
        // The add-task dialog.
        let mut app = app_with(TASKS);
        app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        open_dialog(&mut app);
        assert_eq!(app.on_key(key(KeyCode::BackTab)), Action::None);
        assert!(app.dialog_open, "the dialog keeps the keyboard");
        assert_eq!(app.run_mode(), "sprint");
        assert_eq!(app.status, None);

        // The settings overlay.
        let mut app = app_with(TASKS);
        assert_eq!(app.on_key(key(KeyCode::Char('?'))), Action::None);
        assert!(app.overlay.open);
        assert_eq!(app.on_key(key(KeyCode::BackTab)), Action::None);
        assert!(app.overlay.open, "the overlay keeps the keyboard");
        assert_eq!(app.run_mode(), "sprint");
        assert_eq!(app.status, None);

        // The theme picker modal.
        let mut app = app_with(TASKS);
        assert_eq!(app.on_key(key(KeyCode::Char('t'))), Action::None);
        assert!(app.theme_modal.open);
        assert_eq!(app.on_key(key(KeyCode::BackTab)), Action::None);
        assert!(app.theme_modal.open, "the picker keeps the keyboard");
        assert_eq!(app.run_mode(), "sprint");

        // The stop dialog (Esc while a build runs).
        let mut app = app_with(TASKS);
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        app.on_key(key(KeyCode::Esc));
        assert!(app.stop_open);
        assert_eq!(app.on_key(key(KeyCode::BackTab)), Action::None);
        assert!(app.stop_open, "the stop dialog keeps the keyboard");
        assert_eq!(app.run_mode(), "sprint");
    }

    #[test]
    fn the_run_mode_chip_follows_the_engine_readout() {
        let mut app = app_with(TASKS);
        app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        app.settings
            .insert("run_mode".into(), SettingValue::Str("continuous".into()));
        let screen = draw(&app, 80, 14);
        assert!(screen.contains("continuous"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn idle_status_bar_hint_follows_the_task_list() {
        let mut app = app_with(TASKS);
        app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        let explore = draw(&app, 80, 14);
        let strip = explore.lines().last().unwrap();
        // Pending tasks remain, so Enter starts the build loop (T46.1).
        assert!(strip.contains(" Enter "), "{strip}");
        assert!(strip.contains(" start "), "{strip}");
        assert!(!strip.contains(" run discovery "), "{strip}");
        // The old add-task title wording is gone from the strip.
        assert!(!strip.contains("What do you want to do?"), "{strip}");
        // The add-task chip left the status line with T73.1; the `a` key
        // keeps opening the dialog and the pane hints strip advertises it.
        assert!(!strip.contains(" a "), "{strip}");
        assert!(!strip.contains(" add tasks "), "{strip}");

        // A complete queue flips the label: Enter runs a discovery round.
        let mut complete = app_with("- [x] T1.1: first done\n- [x] T1.2: second done\n");
        complete.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        let screen = draw(&complete, 80, 14);
        let strip = screen.lines().last().unwrap();
        assert!(strip.contains(" Enter "), "{strip}");
        assert!(strip.contains(" run discovery "), "{strip}");
        assert!(!strip.contains(" start "), "{strip}");

        // An empty task list is complete too: Enter runs a discovery round.
        let mut empty = app_with("");
        empty.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        let screen = draw(&empty, 80, 14);
        let strip = screen.lines().last().unwrap();
        assert!(strip.contains(" Enter "), "{strip}");
        assert!(strip.contains(" run discovery "), "{strip}");

        // The leading Enter hint stays while the engine is idle, whatever frame is
        // focused, and leaves once a run starts -- the busy strip shows the
        // Esc stop hint instead.
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, FrameFocus::Output);
        let focused_output = draw(&app, 80, 14);
        let strip = focused_output.lines().last().unwrap();
        assert!(strip.contains(" Enter "), "{strip}");
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        let running = draw(&app, 80, 14);
        let strip = running.lines().last().unwrap();
        assert!(!strip.contains(" Enter "), "{strip}");
        // The running strip keeps only the menu chip: the Esc stop
        // hint moved behind the m menu (T128.1).
        assert!(!strip.contains(" Esc "), "{strip}");
        assert!(!strip.contains(" stop build "), "{strip}");
        assert!(strip.contains(" m "), "{strip}");
        assert!(strip.contains(" menu"), "{strip}");
    }

    #[test]
    fn the_explore_strip_is_gone_from_the_merged_view() {
        let mut app = app_with(TASKS);
        app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        // The strip's scenario headline never renders, idle or busy.
        assert!(!draw(&app, 80, 14).contains("Work is ready to continue."));
        app.apply(EngineEvent::PlanningChanged { planning: true });
        assert!(!draw(&app, 80, 14).contains("Work is ready to continue."));
        app.apply(EngineEvent::PlanningChanged { planning: false });
        assert!(!draw(&app, 80, 14).contains("Work is ready to continue."));
    }

    #[test]
    fn the_primary_button_label_follows_the_scenario() {
        let with_label = |tasks: &str, text: &str| {
            let mut app = app_with(tasks);
            app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
            open_dialog(&mut app);
            type_text(&mut app, text);
            app.primary_label()
        };
        // A ready queue with an empty input offers to start.
        assert_eq!(with_label(TASKS, ""), "Start");
        // A complete queue with an empty input offers to scan.
        assert_eq!(with_label("- [x] T1.1: done\n", ""), "Scan");
        // Any text, and every other scenario, submit.
        assert_eq!(with_label(TASKS, "add a login page"), "Submit");
        assert_eq!(with_label("", ""), "Submit");
        assert_eq!(with_label("- [x] T1.1: done\n", "more work"), "Submit");
    }

    #[test]
    fn dialog_shows_the_start_button_when_the_queue_is_ready() {
        let mut app = app_with(TASKS);
        app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        open_dialog(&mut app);
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("Start"), "{screen}");
        insta::assert_snapshot!(screen);
    }

    #[test]
    fn empty_project_saves_the_brief_and_keeps_the_dialog_open() {
        let mut app = app_with("");
        app.project_status = status(false, TaskFileState::Missing, SpecState::Missing);
        open_dialog(&mut app);
        type_text(&mut app, "A web service for recipes.");
        assert_eq!(
            submit(&mut app),
            Action::SaveBrief("A web service for recipes.".into())
        );
        assert!(app.dialog_open, "the dialog stays open with its message");
        // An empty input is refused: an empty project needs direction.
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.dialog_text, "");
        assert_eq!(submit(&mut app), Action::None);
        assert_eq!(
            app.dialog_status.as_deref(),
            Some("Describe what you want to build -- an empty project needs direction.")
        );
    }

    #[test]
    fn needs_queue_with_an_empty_input_bootstraps_only_with_spec_content() {
        let mut app = app_with("");
        app.project_status = status(true, TaskFileState::Missing, SpecState::Content);
        open_dialog(&mut app);
        // The empty submit starts the research agent's bootstrap queue-creation
        // run (T69.1): it investigates the project and creates the queue.
        assert_eq!(submit(&mut app), Action::ResearchQueue(QueueRun::Bootstrap));
        // Without non-heading spec content the bootstrap run is refused.
        let mut app = app_with("");
        app.project_status = status(true, TaskFileState::Missing, SpecState::Empty);
        open_dialog(&mut app);
        assert_eq!(submit(&mut app), Action::None);
        assert_eq!(
            app.dialog_status.as_deref(),
            Some("SPEC.md is empty -- describe what you want to build first.")
        );
    }

    #[test]
    fn queue_ready_with_an_empty_input_starts_the_build_loop() {
        let mut app = app_with(TASKS);
        app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        open_dialog(&mut app);
        assert_eq!(submit(&mut app), Action::StartBuild);
        assert!(!app.dialog_open, "starting closes the modal");
        assert_eq!(
            app.focus,
            FrameFocus::Output,
            "starting focuses the output frame"
        );
    }

    #[test]
    fn queue_complete_with_an_empty_input_starts_a_scan_run() {
        let mut app = app_with("- [x] T1.1: done\n");
        app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
        open_dialog(&mut app);
        // The empty submit starts the research agent's gap-scan queue-creation
        // run (T69.1).
        assert_eq!(submit(&mut app), Action::ResearchQueue(QueueRun::Scan));
    }

    #[test]
    fn any_non_empty_input_appends_tasks_in_every_scenario() {
        for tasks in ["", TASKS, "- [x] T1.1: done\n"] {
            let mut app = app_with(tasks);
            app.project_status = status(true, TaskFileState::Ok, SpecState::Content);
            open_dialog(&mut app);
            type_text(&mut app, "add a login page");
            assert_eq!(
                submit(&mut app),
                Action::SubmitTasks("add a login page".into()),
                "tasks: {tasks:?}"
            );
        }
    }

    #[test]
    fn a_saved_brief_re_detects_as_needs_queue() {
        let mut app = app_with("");
        app.project_status = status(false, TaskFileState::Missing, SpecState::Missing);
        open_dialog(&mut app);
        type_text(&mut app, "A web service for recipes.");
        assert!(matches!(submit(&mut app), Action::SaveBrief(_)));
        // The driver writes SPEC.md and re-probes before the dialog learns the outcome.
        app.project_status = status(true, TaskFileState::Missing, SpecState::Content);
        app.on_brief_saved();
        assert_eq!(app.scenario(), Scenario::NeedsQueue);
        assert!(app.dialog_open);
        assert_eq!(app.dialog_text, "");
        assert_eq!(
            app.dialog_status.as_deref(),
            Some("SPEC.md created -- review it, then press Enter to start.")
        );
        // The next empty submit takes the NeedsQueue bootstrap path; the label stays
        // "Submit".
        assert_eq!(app.primary_label(), "Submit");
        assert_eq!(submit(&mut app), Action::ResearchQueue(QueueRun::Bootstrap));
    }

    #[test]
    fn enter_runs_discovery_and_a_still_opens_the_dialog() {
        // A complete queue: Enter runs a discovery round (T46.1). With pending
        // tasks Enter starts the build loop instead (covered by the
        // key-mapping tests).
        let mut app = app_with("- [x] T1.1: first done\n- [x] T1.2: second done\n");
        assert_eq!(app.on_key(key(KeyCode::Enter)), Action::RunDiscovery);
        assert!(!app.dialog_open);
        assert!(app.status.is_none());
        // The old `s` key no longer opens anything either.
        assert_eq!(app.on_key(key(KeyCode::Char('s'))), Action::None);
        assert!(!app.dialog_open);
        // `a` stays the add-task dialog key.
        assert_eq!(app.on_key(key(KeyCode::Char('a'))), Action::None);
        assert!(app.dialog_open);
        app.on_key(key(KeyCode::Esc));
        assert!(!app.dialog_open);

        // While busy, Enter refuses with the same messages as the `a` path; each busy
        // case is checked on its own, from an idle app.
        for (apply, reset, message) in [
            (
                EngineEvent::PhaseChanged {
                    phase: Phase::Running,
                },
                EngineEvent::PhaseChanged {
                    phase: Phase::Startup,
                },
                "A build is running; wait for it to finish.",
            ),
            (
                EngineEvent::PlanningChanged { planning: true },
                EngineEvent::PlanningChanged { planning: false },
                "The planner is already running.",
            ),
            (
                EngineEvent::DiscoveryChanged { discovering: true },
                EngineEvent::DiscoveryChanged { discovering: false },
                "A discovery round is running; wait for it to finish.",
            ),
        ] {
            app.apply(apply);
            assert_eq!(app.on_key(key(KeyCode::Enter)), Action::None);
            assert!(
                !app.dialog_open,
                "Enter while busy must not open the dialog"
            );
            assert_eq!(app.status.as_deref(), Some(message));
            // `a` shows the same refusal while busy, but only from the
            // focused task list frame (T74.1); the run moved the focus to
            // the agent output frame, so Tab puts it back first.
            app.on_key(key(KeyCode::Tab));
            app.on_key(key(KeyCode::Char('a')));
            assert!(!app.dialog_open);
            assert_eq!(app.status.as_deref(), Some(message));
            // Back to idle, so the next case starts from an idle app.
            app.apply(reset);
            assert!(app.is_idle());
        }
    }
}

/// The merged view (T30.1): one screen with the agent output frame stacked above
/// the task list frame, sized by the engine state and the frame focus.
mod view {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use patok_tui::FrameFocus;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn wheel(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// 20 done tasks, then 20 pending ones, so the auto-follow offset is positive.
    fn long_tasks() -> String {
        let mut text = String::new();
        for n in 1..=20 {
            text.push_str(&format!("- [x] T9.{n}: done thing {n}\n"));
        }
        for n in 21..=40 {
            text.push_str(&format!("- [ ] T9.{n}: later thing {n}\n"));
        }
        text
    }

    /// An app with the long queue and 30 lines of agent output, so both frames
    /// have something to scroll.
    fn long_app() -> App {
        let mut app = app_with(&long_tasks());
        for n in 1..=30 {
            app.apply(agent(AgentEvent::Text {
                text: format!("output line {n}"),
            }));
        }
        app
    }

    fn app_with(tasks: &str) -> App {
        App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: task::parse(tasks),
                current_task: None,
                planning: false,
                discovering: false,
                provider: String::new(),
                model: String::new(),
                settings: BTreeMap::new(),
                pipeline: PipelineState::today(),
                recent: vec![],
            },
            "0.1.0".into(),
        )
    }

    #[test]
    fn idle_defaults_to_the_task_list_frame() {
        let app = long_app();
        assert_eq!(app.focus, FrameFocus::Tasks);
        draw(&app, 80, 24);
        let output = app.output_area.get();
        let tasks = app.tasks_area.get();
        // The rail's fixed column (T53.1) sits left of the frames in the idle
        // state too, so the unfocused output frame shrinks to 5 content lines
        // plus its border beside it.
        assert_eq!((output.y, output.height), (0, 7));
        // The focused task list takes the rest, stacked below the output frame.
        assert_eq!((tasks.y, tasks.height), (7, 16));
        assert_eq!(tasks.x, output.x);
    }

    #[test]
    fn a_running_build_defaults_to_the_output_frame() {
        let mut app = long_app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        assert_eq!(app.focus, FrameFocus::Output);
        draw(&app, 80, 24);
        let output = app.output_area.get();
        let tasks = app.tasks_area.get();
        assert_eq!((output.y, output.height), (0, 16));
        assert_eq!((tasks.y, tasks.height), (16, 7));
        // The rail column (T50.1) shifts both frames right by its width --
        // the normal-mode column, the default since T58.1.
        let rail = patok_tui::rail_width(patok_core::config::RailMode::Normal);
        assert_eq!((output.x, tasks.x), (rail, rail));
    }

    #[test]
    fn planner_and_discovery_runs_default_to_the_output_frame() {
        let mut app = long_app();
        app.apply(EngineEvent::PlanningChanged { planning: true });
        assert_eq!(app.focus, FrameFocus::Output);
        app.apply(EngineEvent::PlanningChanged { planning: false });
        assert_eq!(app.focus, FrameFocus::Tasks);
        app.apply(EngineEvent::DiscoveryChanged { discovering: true });
        assert_eq!(app.focus, FrameFocus::Output);
        app.apply(EngineEvent::DiscoveryChanged { discovering: false });
        assert_eq!(app.focus, FrameFocus::Tasks);
    }

    #[test]
    fn the_split_is_never_equal_halves() {
        use patok_tui::frame_constraints;
        use ratatui::layout::Constraint;
        assert_eq!(
            frame_constraints(FrameFocus::Output),
            [Constraint::Min(1), Constraint::Length(7)]
        );
        assert_eq!(
            frame_constraints(FrameFocus::Tasks),
            [Constraint::Length(7), Constraint::Min(1)]
        );
    }

    #[test]
    fn tab_switches_the_focus_and_resizes_the_frames() {
        let mut app = long_app();
        draw(&app, 80, 24);
        assert_eq!(app.focus, FrameFocus::Tasks);
        app.on_key(key(KeyCode::Tab));
        draw(&app, 80, 24);
        assert_eq!(app.focus, FrameFocus::Output);
        // The rail's column costs the body no rows (T53.1), so the focused
        // output frame takes what is left above the fixed task list frame.
        assert_eq!(
            (app.output_area.get().height, app.tasks_area.get().height),
            (16, 7)
        );
        app.on_key(key(KeyCode::Tab));
        draw(&app, 80, 24);
        assert_eq!(app.focus, FrameFocus::Tasks);
        assert_eq!(
            (app.output_area.get().height, app.tasks_area.get().height),
            (7, 16)
        );
    }

    #[test]
    fn a_mouse_click_focuses_the_frame_it_lands_in() {
        let mut app = long_app();
        draw(&app, 80, 24);
        let output = app.output_area.get();
        let tasks = app.tasks_area.get();
        assert_eq!(app.focus, FrameFocus::Tasks);
        app.on_mouse(click(output.x + 1, output.y + 2));
        assert_eq!(app.focus, FrameFocus::Output);
        app.on_mouse(click(tasks.x + 2, tasks.y + 3));
        assert_eq!(app.focus, FrameFocus::Tasks);
        // Clicks on the rail or the status line change nothing.
        app.on_mouse(click(0, 0));
        assert_eq!(app.focus, FrameFocus::Tasks);
        app.on_mouse(click(0, 23));
        assert_eq!(app.focus, FrameFocus::Tasks);
        // Other buttons and other mouse events change nothing.
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            ..click(output.x + 1, output.y + 2)
        });
        assert_eq!(app.focus, FrameFocus::Tasks);
        // An open modal swallows every click.
        app.on_key(key(KeyCode::Char('a')));
        assert!(app.dialog_open);
        app.on_mouse(click(output.x + 1, output.y + 1));
        assert_eq!(app.focus, FrameFocus::Tasks);
    }

    #[test]
    fn up_down_scroll_only_the_focused_frame() {
        let mut app = long_app();
        draw(&app, 80, 24);
        // Idle: the task list is focused, so Up and Down move only the task scroll.
        assert!(app.task_max_scroll.get() > 0);
        // The auto view starts at the 17th task (the first pending one a third
        // down of the taller idle task list beside the rail column).
        assert!(draw(&app, 80, 24).contains("T9.17"));
        app.on_key(key(KeyCode::Up));
        assert_eq!((app.task_scroll, app.scroll), (1, 0));
        app.on_key(key(KeyCode::Char('k')));
        assert_eq!((app.task_scroll, app.scroll), (2, 0));
        // Scrolled up two rows, the view reaches the 15th task.
        assert!(draw(&app, 80, 24).contains("T9.15"));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.task_scroll, 0);

        // Busy: the output frame is focused; Up and Down move only the output scroll.
        let task_scroll = app.task_scroll;
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        draw(&app, 80, 24);
        assert!(app.max_scroll.get() > 0);
        app.on_key(key(KeyCode::Up));
        assert_eq!((app.scroll, app.task_scroll), (1, task_scroll));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.scroll, 0);

        // Tab moves the keys back to the task list, leaving the output scroll alone.
        app.on_key(key(KeyCode::Tab));
        draw(&app, 80, 24);
        app.on_key(key(KeyCode::Up));
        assert_eq!((app.task_scroll, app.scroll), (task_scroll + 1, 0));
    }

    #[test]
    fn the_hit_test_maps_the_pointer_to_the_frame_it_is_over() {
        use patok_tui::frame_at;
        use ratatui::layout::{Position, Rect};
        // The real idle layout at 80x24 (T53.1, T86.1): the rail's fixed
        // column on the left, the output frame at rows 0-6 beside it, the
        // task list frame at rows 7-22, the status line below.
        let output = Rect::new(6, 0, 74, 7);
        let tasks = Rect::new(6, 7, 74, 16);
        let at = |column, row| frame_at(Position::new(column, row), output, tasks);
        // Inside the output frame: the top-left corner, the middle, the bottom row.
        assert_eq!(at(6, 0), Some(FrameFocus::Output));
        assert_eq!(at(10, 3), Some(FrameFocus::Output));
        assert_eq!(at(6, 6), Some(FrameFocus::Output));
        // Inside the task list frame: the shared boundary row below the output
        // frame belongs to it alone, so do the middle and the bottom-right corner.
        assert_eq!(at(6, 7), Some(FrameFocus::Tasks));
        assert_eq!(at(10, 15), Some(FrameFocus::Tasks));
        assert_eq!(at(79, 22), Some(FrameFocus::Tasks));
        // Outside both frames: the rail's column, the status line, and a
        // column past the frames' right edge.
        assert_eq!(at(0, 0), None);
        assert_eq!(at(0, 23), None);
        assert_eq!(at(2, 5), None);
        assert_eq!(at(80, 5), None);
    }

    #[test]
    fn a_wheel_step_scrolls_only_the_frame_the_pointer_is_over() {
        let mut app = long_app();
        draw(&app, 80, 24);
        let output = app.output_area.get();
        let tasks = app.tasks_area.get();
        assert_eq!(app.focus, FrameFocus::Tasks);
        assert!(app.task_max_scroll.get() > 0);
        assert!(app.max_scroll.get() > 0);

        // A wheel step over the task list scrolls only the task list.
        app.on_mouse(wheel(MouseEventKind::ScrollUp, tasks.x + 2, tasks.y + 3));
        assert_eq!((app.task_scroll, app.scroll), (1, 0));
        assert_eq!(app.focus, FrameFocus::Tasks);

        // A wheel step over the agent output scrolls only the output, even
        // though the task list holds the focus, and the focus stays put.
        app.on_mouse(wheel(MouseEventKind::ScrollUp, output.x + 1, output.y + 2));
        assert_eq!((app.task_scroll, app.scroll), (1, 1));
        assert_eq!(app.focus, FrameFocus::Tasks);

        // Wheel events on the rail and the status line change nothing.
        app.on_mouse(wheel(MouseEventKind::ScrollUp, 0, 0));
        app.on_mouse(wheel(MouseEventKind::ScrollDown, 0, 23));
        assert_eq!((app.task_scroll, app.scroll), (1, 1));

        // ScrollDown walks both offsets back to their auto-follow view.
        app.on_mouse(wheel(MouseEventKind::ScrollDown, tasks.x + 1, tasks.y + 1));
        assert_eq!((app.task_scroll, app.scroll), (0, 1));
        app.on_mouse(wheel(
            MouseEventKind::ScrollDown,
            output.x + 1,
            output.y + 1,
        ));
        assert_eq!((app.task_scroll, app.scroll), (0, 0));

        // Clamping: many steps up over the task list stop at its top, and many
        // steps down stop at the auto-follow view.
        for _ in 0..100 {
            app.on_mouse(wheel(MouseEventKind::ScrollUp, tasks.x + 2, tasks.y + 3));
        }
        assert_eq!(app.task_scroll, app.task_max_scroll.get());
        for _ in 0..100 {
            app.on_mouse(wheel(MouseEventKind::ScrollDown, tasks.x + 2, tasks.y + 3));
        }
        assert_eq!(app.task_scroll, 0);
        // The same clamp on the output frame: up stops at its top (the oldest
        // line), down resumes the live bottom.
        for _ in 0..100 {
            app.on_mouse(wheel(MouseEventKind::ScrollUp, output.x + 1, output.y + 2));
        }
        assert_eq!(app.scroll, app.max_scroll.get());
        for _ in 0..100 {
            app.on_mouse(wheel(
                MouseEventKind::ScrollDown,
                output.x + 1,
                output.y + 2,
            ));
        }
        assert_eq!(app.scroll, 0);

        // Auto-follow pause, exactly like the Up key: a wheel step over the
        // output holds the view while new output streams in, and a step back
        // down resumes the live bottom.
        assert!(draw(&app, 80, 24).contains("output line 30"));
        app.on_mouse(wheel(MouseEventKind::ScrollUp, output.x + 1, output.y + 2));
        assert_eq!(app.scroll, 1);
        app.apply(agent(AgentEvent::Text {
            text: "output line 31".into(),
        }));
        let screen = draw(&app, 80, 24);
        assert!(screen.contains("output line 30"));
        assert!(!screen.contains("output line 31"));
        app.on_mouse(wheel(
            MouseEventKind::ScrollDown,
            output.x + 1,
            output.y + 2,
        ));
        assert_eq!(app.scroll, 0);
        assert!(draw(&app, 80, 24).contains("output line 31"));

        // The focus never moved through any of it, and a click still moves it.
        assert_eq!(app.focus, FrameFocus::Tasks);
        app.on_mouse(click(output.x + 1, output.y + 2));
        assert_eq!(app.focus, FrameFocus::Output);
    }

    #[test]
    fn wheel_scrolled_frames_render_their_scrolled_content() {
        let mut app = long_app();
        draw(&app, 80, 24);
        let output = app.output_area.get();
        let tasks = app.tasks_area.get();
        // Two wheel steps over the task list move it up two rows while the
        // output frame sits at its live bottom.
        app.on_mouse(wheel(MouseEventKind::ScrollUp, tasks.x + 2, tasks.y + 3));
        app.on_mouse(wheel(MouseEventKind::ScrollUp, tasks.x + 2, tasks.y + 3));
        assert_eq!((app.task_scroll, app.scroll), (2, 0));
        let tasks_scrolled = draw(&app, 80, 24);
        assert!(tasks_scrolled.contains("T9.15"));
        insta::assert_snapshot!(tasks_scrolled);
        // The reverse: two wheel steps over the output hold its view two lines
        // above the live bottom while the task list stays at its auto view.
        app.on_mouse(wheel(MouseEventKind::ScrollUp, output.x + 1, output.y + 2));
        app.on_mouse(wheel(MouseEventKind::ScrollUp, output.x + 1, output.y + 2));
        assert_eq!((app.task_scroll, app.scroll), (2, 2));
        let output_scrolled = draw(&app, 80, 24);
        assert!(output_scrolled.contains("output line 28"));
        insta::assert_snapshot!(output_scrolled);
    }

    #[test]
    fn one_header_two_frames_and_no_explore_strip() {
        let mut app = app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        app.apply(EngineEvent::TaskStarted {
            id: "T1.2".into(),
            description: "add the parser for task files".into(),
        });
        app.apply(agent(AgentEvent::Text {
            text: "working on it".into(),
        }));
        let screen = draw(&app, 80, 24);
        let lines: Vec<&str> = screen.lines().collect();
        // The merged status line (T86.1): the chips lead the bottom line.
        assert!(lines[23].starts_with(" RUNNING  sprint"), "{screen}");
        // The rail's Plan box sits in the fixed column left of the frames
        // (T50.1, the normal-mode default since T58.1), one row below the
        // rail's blank top row (T63.1); the output frame starts beside the
        // blank row on the frame's first row (the header row is gone).
        assert!(lines[0].trim_start().starts_with("┌ Builder"));
        assert!(lines[1].contains("[ Research ]"));
        let tasks = app.tasks_area.get();
        assert!(lines[usize::from(tasks.y)].contains("┌ Tasks | 1/3 - 2 left"));
        assert!(screen.contains("working on it"));
        assert!(screen.contains("T1.2"));
        // No explore strip content anywhere.
        assert!(!screen.contains("Work is ready to continue."));
        assert!(!screen.contains("SPEC.md"));
        insta::assert_snapshot!(screen);
    }
}

/// The focused dashboard frame's key hints strip (T71.1): the frame that owns
/// the keyboard reserves the bottom row inside its borders for a one-line
/// list of its keys -- every label matching what `App::on_key` does with the
/// key -- rendered in the theme's low-emphasis footer colour so it recolours
/// with the active theme. The unfocused frame has no strip at all, so exactly
/// one strip shows at any time, Tab swaps it between the two frames without
/// triggering any pane action, and a narrow frame sheds its trailing hints
/// without ever touching the borders.
mod hints {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_core::config::Theme as ThemeKey;
    use patok_tui::{Action, FrameFocus, Theme, fitted_hints};
    use ratatui::layout::Rect;

    const W: u16 = 80;
    const H: u16 = 24;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn buffer(app: &App) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The bottom row inside one frame's borders: the row the hints strip
    /// occupies when that frame is focused.
    fn bottom_row(area: Rect) -> Rect {
        Rect::new(area.x + 1, area.y + area.height - 2, area.width - 2, 1)
    }

    /// One row's text between the frame's borders.
    fn row_text(buffer: &ratatui::buffer::Buffer, row: Rect) -> String {
        (row.x..row.x + row.width)
            .map(|x| buffer[(x, row.y)].symbol())
            .collect()
    }

    /// Asserts `text` is rendered on `row` in the theme's footer colour, cell
    /// by cell, so the strip is pinned by its style and not just its text.
    fn assert_strip(
        buffer: &ratatui::buffer::Buffer,
        row: Rect,
        text: &str,
        colour: ratatui::style::Color,
    ) {
        let rendered = row_text(buffer, row);
        assert_eq!(rendered.trim_end(), text, "on row y={}", row.y);
        for (i, ch) in text.chars().enumerate() {
            let cell = &buffer[(row.x + i as u16, row.y)];
            assert_eq!(cell.symbol(), ch.to_string());
            assert_eq!(
                cell.style().fg,
                Some(colour),
                "hint cell {i} wears the footer colour"
            );
        }
    }

    fn running() -> App {
        let mut app = app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        app
    }

    fn themed(mut app: App, theme: ThemeKey) -> App {
        app.tui.theme = theme;
        app.tui.truecolor = Some(true);
        app
    }

    /// The idle task list strip with every hint that fits the 80-column
    /// frame (T75.1 dropped the idle `q quit` entry; T76.1 added the `i
    /// inject task` entry, whose width makes the trailing `Tab output` hint
    /// drop at 80 columns -- the leading hints stay).
    const TASKS_STRIP: &str = " a add tasks · i inject task · Enter start · ↑↓ scroll";
    /// The focused output frame's strip with every hint that fits the
    /// 80-column frame (T124.1 dropped the leading `v rail view` entry,
    /// which makes the trailing `Tab tasks` hint fit; T125.1 removed the
    /// `v` binding itself).
    const OUTPUT_STRIP: &str = " ↑↓ scroll · PgUp/PgDn page · End follow · Tab tasks";

    #[test]
    fn the_focused_task_list_shows_its_hints_along_its_bottom_edge() {
        let app = app();
        assert_eq!(app.focus, FrameFocus::Tasks);
        let buffer = buffer(&app);
        let theme = Theme::DARK;
        assert_strip(
            &buffer,
            bottom_row(app.tasks_area.get()),
            TASKS_STRIP,
            theme.muted_text,
        );
        // The idle task list strip no longer advertises the quit key (T75.1)
        // -- pressing `q` still quits, the hint line just does not say so.
        let rendered = row_text(&buffer, bottom_row(app.tasks_area.get()));
        assert!(!rendered.contains('q'), "the tasks strip lists no q hint");
        assert!(
            !rendered.contains("quit"),
            "the tasks strip lists no quit label"
        );
        // The tasks strip advertises the inject key (T76.1)...
        assert!(
            rendered.contains("i inject task"),
            "the tasks strip lists the inject hint"
        );
        // ...and the output frame's strip never claims it.
        assert!(
            !OUTPUT_STRIP.contains("inject"),
            "the output strip lists no inject hint"
        );
        assert_eq!(
            OUTPUT_STRIP,
            " ↑↓ scroll · PgUp/PgDn page · End follow · Tab tasks"
        );
        // The unfocused output frame has no strip, so exactly one of the two
        // frames shows hints at any time.
        let output_row = bottom_row(app.output_area.get());
        assert!(row_text(&buffer, output_row).trim().is_empty());
        // The strip stays clear of the frame's borders.
        let tasks = app.tasks_area.get();
        let row = bottom_row(tasks);
        assert_eq!(buffer[(tasks.x, row.y)].symbol(), "│");
        assert_eq!(buffer[(tasks.right() - 1, row.y)].symbol(), "│");
    }

    #[test]
    fn the_focused_output_frame_shows_its_hints_along_its_bottom_edge() {
        let app = running();
        assert_eq!(app.focus, FrameFocus::Output);
        let buffer = buffer(&app);
        let theme = Theme::DARK;
        assert_strip(
            &buffer,
            bottom_row(app.output_area.get()),
            OUTPUT_STRIP,
            theme.muted_text,
        );
        // The unfocused task list frame has no strip.
        let tasks_row = bottom_row(app.tasks_area.get());
        assert!(row_text(&buffer, tasks_row).trim().is_empty());
    }

    #[test]
    fn the_strip_recolours_with_the_active_theme() {
        let app = themed(app(), ThemeKey::TokyoNightDark);
        let theme = Theme::resolve(ThemeKey::TokyoNightDark, Some(true));
        let buffer = buffer(&app);
        assert_strip(
            &buffer,
            bottom_row(app.tasks_area.get()),
            TASKS_STRIP,
            theme.muted_text,
        );
        // The palette theme's footer colour differs from the default one, so
        // the strip really follows the theme and no literal is baked in.
        assert_ne!(theme.muted_text, Theme::DARK.muted_text);
    }

    #[test]
    fn tab_swaps_the_strip_between_the_frames_without_any_pane_action() {
        let mut app = app();
        buffer(&app);
        let (scroll, task_scroll) = (app.scroll, app.task_scroll);
        let (max_scroll, task_max_scroll) = (app.max_scroll.get(), app.task_max_scroll.get());
        assert_eq!(app.on_key(key(KeyCode::Tab)), Action::None);
        assert_eq!(app.focus, FrameFocus::Output);
        // The toggle is not a pane action: no scroll moved, no dialog or
        // modal opened, no status message, no stop.
        assert_eq!((app.scroll, app.task_scroll), (scroll, task_scroll));
        assert_eq!(
            (app.max_scroll.get(), app.task_max_scroll.get()),
            (max_scroll, task_max_scroll)
        );
        assert!(!app.dialog_open);
        assert!(!app.stop_open);
        assert!(!app.overlay.open);
        assert!(!app.stopping);
        assert!(app.status.is_none());
        let drawn = buffer(&app);
        assert_strip(
            &drawn,
            bottom_row(app.output_area.get()),
            OUTPUT_STRIP,
            Theme::DARK.muted_text,
        );
        // The strip left the task list the moment the focus did.
        let tasks_row = bottom_row(app.tasks_area.get());
        assert!(row_text(&drawn, tasks_row).trim().is_empty());

        // And back: the output frame's strip disappears the same way.
        assert_eq!(app.on_key(key(KeyCode::Tab)), Action::None);
        assert_eq!(app.focus, FrameFocus::Tasks);
        let drawn = buffer(&app);
        assert_strip(
            &drawn,
            bottom_row(app.tasks_area.get()),
            TASKS_STRIP,
            Theme::DARK.muted_text,
        );
        let output_row = bottom_row(app.output_area.get());
        assert!(row_text(&drawn, output_row).trim().is_empty());
    }

    #[test]
    fn every_advertised_key_does_what_its_hint_claims() {
        // The task list strip's keys, on an idle engine with a pending queue.
        {
            let mut app = app();
            assert_eq!(app.on_key(key(KeyCode::Char('a'))), Action::None);
            assert!(app.dialog_open);
            app.on_key(key(KeyCode::Esc));
            assert!(!app.dialog_open);
        }
        {
            let mut app = app();
            assert_eq!(app.on_key(key(KeyCode::Char('i'))), Action::None);
            assert!(app.dialog_open);
            app.on_key(key(KeyCode::Esc));
            assert!(!app.dialog_open);
        }
        {
            let mut app = app();
            assert_eq!(app.on_key(key(KeyCode::Enter)), Action::StartBuild);
        }
        // A complete queue points Enter at a discovery round instead.
        {
            let mut complete = app();
            complete.apply(EngineEvent::TasksChanged {
                tasks: task::parse(&TASKS.replace("[ ]", "[x]")),
            });
            assert_eq!(complete.on_key(key(KeyCode::Enter)), Action::RunDiscovery);
        }
        // `q` is no longer advertised in the idle task list strip (T75.1),
        // but the key still quits exactly as before.
        {
            let mut app = app();
            assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::Interrupt);
        }

        // The output strip's keys, while a build runs.
        let mut app = running();
        for n in 1..=40 {
            app.apply(agent(AgentEvent::Text {
                text: format!("output line {n}"),
            }));
        }
        buffer(&app);
        assert!(app.max_scroll.get() > 0);
        assert_eq!(app.on_key(key(KeyCode::Up)), Action::None);
        assert_eq!(app.scroll, 1);
        assert_eq!(app.on_key(key(KeyCode::PageUp)), Action::None);
        assert_eq!(app.scroll, 11);
        assert_eq!(app.on_key(key(KeyCode::PageDown)), Action::None);
        assert_eq!(app.scroll, 1);
        assert_eq!(app.on_key(key(KeyCode::End)), Action::None);
        assert_eq!(app.scroll, 0);

        // The busy task list strip's labels: Enter is not advertised (it is
        // refused), and q soft-stops instead of quitting.
        let mut app = running();
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, FrameFocus::Tasks);
        assert_eq!(app.on_key(key(KeyCode::Char('q'))), Action::Quit);
    }

    #[test]
    fn fitted_hints_keep_the_leading_hints_that_fit() {
        let hints = [
            "a add tasks",
            "i inject task",
            "Enter start",
            "↑↓ scroll",
            "Tab output",
        ];
        // One leading space plus ` · ` between hints: the cumulative widths
        // are 12, 28, 42, 54 and 67 columns.
        assert!(fitted_hints(&hints, 0).is_empty());
        assert!(fitted_hints(&hints, 11).is_empty());
        assert_eq!(fitted_hints(&hints, 12), ["a add tasks"]);
        assert_eq!(fitted_hints(&hints, 27), ["a add tasks"]);
        assert_eq!(fitted_hints(&hints, 28), ["a add tasks", "i inject task"]);
        assert_eq!(fitted_hints(&hints, 41), ["a add tasks", "i inject task"]);
        assert_eq!(
            fitted_hints(&hints, 42),
            ["a add tasks", "i inject task", "Enter start"]
        );
        assert_eq!(
            fitted_hints(&hints, 53),
            ["a add tasks", "i inject task", "Enter start"]
        );
        assert_eq!(
            fitted_hints(&hints, 54),
            ["a add tasks", "i inject task", "Enter start", "↑↓ scroll"]
        );
        assert_eq!(
            fitted_hints(&hints, 66),
            ["a add tasks", "i inject task", "Enter start", "↑↓ scroll"]
        );
        assert_eq!(fitted_hints(&hints, 67), hints);
    }

    #[test]
    fn a_narrow_frame_drops_trailing_hints_inside_its_borders() {
        // 60 columns leave the focused task list 45 inner columns: the first
        // three hints fit and everything from `↑↓ scroll` drops.
        let app = app();
        let mut terminal = Terminal::new(TestBackend::new(60, H)).unwrap();
        terminal.draw(|frame| render(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let tasks = app.tasks_area.get();
        let row = bottom_row(tasks);
        let rendered = row_text(buffer, row);
        assert_eq!(
            rendered.trim_end(),
            " a add tasks · i inject task · Enter start"
        );
        // The strip never reaches the frame's borders.
        assert_eq!(buffer[(tasks.x, row.y)].symbol(), "│");
        assert_eq!(buffer[(tasks.right() - 1, row.y)].symbol(), "│");
        // The unfocused output frame still has no strip.
        assert!(
            row_text(buffer, bottom_row(app.output_area.get()))
                .trim()
                .is_empty()
        );
    }

    #[test]
    fn hints_strip_in_the_focused_task_list_frame() {
        let app = app();
        insta::assert_snapshot!(draw(&app, 100, 14));
    }

    #[test]
    fn hints_strip_in_the_focused_output_frame() {
        let app = running();
        insta::assert_snapshot!(draw(&app, 100, 14));
    }

    #[test]
    fn narrow_frame_truncates_the_trailing_hints() {
        let app = app();
        insta::assert_snapshot!(draw(&app, 60, 14));
    }
}

/// The stop dialog (T20.1, opened with Esc while a build runs since T46.1),
/// and its three choices map to the soft stop, the interrupt and a plain close.
mod stop_dialog {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_tui::Action;

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(app: &mut App, c: char) -> Action {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    /// A running build with a started task, the state Esc opens the dialog from.
    fn running() -> App {
        let mut app = app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        app.apply(EngineEvent::TaskStarted {
            id: "T1.2".into(),
            description: "add the parser for task files".into(),
        });
        app
    }

    /// The stop dialog opened over a running build (Esc, T46.1).
    fn open() -> App {
        let mut app = running();
        press(&mut app, KeyCode::Esc);
        assert!(app.stop_open, "the dialog must be open");
        app
    }

    #[test]
    fn enter_while_idle_starts_the_build() {
        let mut app = app();
        assert_eq!(press(&mut app, KeyCode::Enter), Action::StartBuild);
        assert!(!app.stop_open);
    }

    #[test]
    fn esc_while_running_opens_the_dialog_instead() {
        let mut app = running();
        // The old `s` key no longer opens the dialog (T46.1).
        assert_eq!(press(&mut app, KeyCode::Char('s')), Action::None);
        assert!(!app.stop_open);
        assert_eq!(app.status, None);
        // Esc opens the stop dialog while a build runs.
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(app.stop_open);
        assert_eq!(app.stop_selected, 0);
        assert_eq!(app.status, None);
        assert_eq!(app.phase, Phase::Running);
        assert!(!app.stopping);
        // A second Esc closes it: no second build, no state change.
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(!app.stop_open);
        assert_eq!(app.phase, Phase::Running);
        assert!(!app.stopping);
    }

    #[test]
    fn up_down_move_the_selection() {
        let mut app = open();
        press(&mut app, KeyCode::Down);
        assert_eq!(app.stop_selected, 1);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.stop_selected, 2);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.stop_selected, 2, "clamped at the last row");
        press(&mut app, KeyCode::Up);
        assert_eq!(app.stop_selected, 1);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.stop_selected, 0);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.stop_selected, 0, "clamped at the first row");
    }

    #[test]
    fn enter_on_soft_stop_soft_stops() {
        let mut app = open();
        assert_eq!(press(&mut app, KeyCode::Enter), Action::Quit);
        assert!(!app.stop_open);
        assert!(app.stopping);
        let status = app.status.as_deref().unwrap();
        assert!(
            status.contains("stops after the current task") && status.contains("app keeps running"),
            "status: {status}"
        );
    }

    #[test]
    fn enter_on_interrupt_interrupts() {
        let mut app = open();
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::Interrupt);
        assert!(!app.stop_open);
        assert!(app.stopping);
        let status = app.status.as_deref().unwrap();
        assert!(
            status.contains("cancelled") && status.contains("keeps running"),
            "status: {status}"
        );
    }

    #[test]
    fn enter_on_cancel_continues() {
        let mut app = open();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert!(!app.stop_open);
        assert!(!app.stopping);
        assert_eq!(app.phase, Phase::Running);
        assert_eq!(app.current_task.as_deref(), Some("T1.2"));
    }

    #[test]
    fn esc_closes_and_the_build_continues() {
        let mut app = open();
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(!app.stop_open);
        assert!(!app.stopping);
        assert_eq!(app.phase, Phase::Running);
        assert_eq!(app.current_task.as_deref(), Some("T1.2"));
    }

    #[test]
    fn other_keys_are_swallowed_while_open() {
        let mut app = open();
        let focus = app.focus;
        for code in [
            KeyCode::Char('a'),
            KeyCode::Char('d'),
            KeyCode::Char('q'),
            KeyCode::Tab,
            KeyCode::Char('j'),
            KeyCode::PageUp,
        ] {
            assert_eq!(press(&mut app, code), Action::None);
            assert!(app.stop_open, "the dialog stays open");
            assert!(!app.dialog_open);
            assert!(!app.stopping);
            assert_eq!(app.focus, focus);
        }
        // Ctrl+C still detaches from anywhere.
        assert_eq!(ctrl(&mut app, 'c'), Action::Detach);
    }

    #[test]
    fn stop_dialog_rect_is_centered_and_inside_the_screen() {
        for (w, h) in [(80u16, 24u16), (30, 10)] {
            let screen = ratatui::layout::Rect::new(0, 0, w, h);
            let rect = patok_tui::stop_area(screen);
            assert!(
                rect.right() <= w && rect.bottom() <= h,
                "{rect:?} in {w}x{h}"
            );
            assert!(rect.width >= 54.min(w));
            assert!(rect.height >= 9.min(h));
            assert!(rect.x.abs_diff(w - rect.right()) <= 1);
            assert!(rect.y.abs_diff(h - rect.bottom()) <= 1);
            // Rendering at the same size must not panic.
            draw(&open(), w, h);
        }
    }

    #[test]
    fn stop_dialog_initial() {
        insta::assert_snapshot!(draw(&open(), 60, 16));
    }

    #[test]
    fn stop_dialog_interrupt_selected() {
        let mut app = open();
        press(&mut app, KeyCode::Down);
        insta::assert_snapshot!(draw(&app, 60, 16));
    }

    #[test]
    fn stop_dialog_cancel_selected() {
        let mut app = open();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        insta::assert_snapshot!(draw(&app, 60, 16));
    }

    /// Renders into an 80x24 TestBackend and returns the buffer, for
    /// cell-level style assertions.
    fn buffer(app: &App) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The selected choice's colours (T118.1): the row under the selection --
    /// its marker, label and detail cells alike -- wears the highlighted-text
    /// foreground on the normal modal background, bold, while the other rows
    /// keep the label on the theme's foreground and the detail in the muted
    /// colour. Re-asserted one Down later, so two selection positions pass.
    #[test]
    fn the_selected_choice_wears_the_highlighted_text_colour() {
        use ratatui::layout::{Constraint, Layout, Rect};
        use ratatui::style::Modifier;

        let area = patok_tui::stop_area(Rect::new(0, 0, 80, 24));
        let inner = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
        let [body, _footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        let mut app = open();
        for round in 0..2 {
            let buffer = buffer(&app);
            let theme = app.theme();
            for index in 0..3u16 {
                let y = body.y + index;
                let row: String = (body.x..body.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                let selected = usize::from(index) == app.stop_selected;
                let text = row.trim_end();
                let chars: Vec<char> = text.chars().collect();
                // The label ends where the muted " -- detail" span begins;
                // none of the labels or details contains a dash.
                let label_end = chars
                    .iter()
                    .position(|&c| c == '-')
                    .expect("the detail follows the label");
                for (i, _) in chars.iter().enumerate() {
                    let cell = &buffer[(body.x + i as u16, y)];
                    if cell.symbol() == " " {
                        continue;
                    }
                    if selected {
                        assert_eq!(
                            cell.style().fg,
                            Some(theme.highlighted_text),
                            "round {round}: selected row {index}'s cell {i} wears the highlight"
                        );
                        assert_eq!(
                            cell.bg, theme.background,
                            "round {round}: selected row {index}'s cell {i} keeps the background"
                        );
                        assert!(
                            cell.style().add_modifier.contains(Modifier::BOLD),
                            "round {round}: selected row {index}'s cell {i} stays bold"
                        );
                    } else {
                        let expected = if i < label_end {
                            theme.foreground
                        } else {
                            theme.muted_text
                        };
                        assert_eq!(
                            cell.style().fg,
                            Some(expected),
                            "round {round}: row {index}'s cell {i} keeps its colour"
                        );
                    }
                }
            }
            assert_eq!(press(&mut app, KeyCode::Down), Action::None);
        }
        assert_eq!(app.stop_selected, 2);
    }
}

/// The modal bottom lines' two-zone layout (T59.1, T64.1): the add-task dialog
/// and the stop dialog render their action buttons right-aligned in a fixed
/// right-hand zone and their key hints left-aligned in a left-hand zone --
/// hints without brackets in the low-emphasis footer colour, buttons with
/// their bracketed key in the button accent and their label in the theme's
/// foreground -- and every button answers a click on its rectangle with
/// exactly its key's action, while hints stay non-interactive.
mod modal_footer {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use patok_core::config::Theme as ThemeKey;
    use patok_tui::{Action, Theme, button_text, footer_button_rects};
    use ratatui::style::Color;
    use ratatui::style::Modifier;

    const W: u16 = 80;
    const H: u16 = 24;

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn buffer(app: &App) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Renders at a width where the hint zone fits every hint (T62.1): at 80
    /// columns the zone gives up its width first (T59.1) and clips the tail
    /// hint, so the whole three-hint line pins at a wider terminal.
    fn buffer_wide(app: &App) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(120, H)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn open_dialog() -> App {
        let mut app = app();
        press(&mut app, KeyCode::Char('a'));
        assert!(app.dialog_open);
        app
    }

    fn open_stop() -> App {
        let mut app = app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        app.apply(EngineEvent::TaskStarted {
            id: "T1.2".into(),
            description: "add the parser for task files".into(),
        });
        press(&mut app, KeyCode::Esc);
        assert!(app.stop_open);
        app
    }

    fn themed(mut app: App, theme: ThemeKey) -> App {
        app.tui.theme = theme;
        app.tui.truecolor = Some(true);
        app
    }

    /// Asserts the two zones on one rendered bottom line (T59.1, T64.1): the
    /// hint text opens the line in the low-emphasis footer colour and without
    /// brackets, the buttons sit right-aligned each exactly the shared
    /// button text with its bracketed key in the button accent and its label
    /// in the theme's foreground (no bold), at least two blank columns
    /// separate the zones, and no cell of the left zone carries the accent or
    /// a bracket.
    fn assert_two_zones(
        buffer: &ratatui::buffer::Buffer,
        footer: ratatui::layout::Rect,
        hints: &str,
        buttons: &[(&str, &str)],
        theme: &Theme,
    ) {
        let row = footer.y;
        let line = |from: u16, to: u16| -> String {
            (from..to).map(|x| buffer[(x, row)].symbol()).collect()
        };

        // The hint zone: the hints open the line, styled with the footer
        // colour, never the accent.
        let hint = format!(" {hints}");
        let hint_width = hint.chars().count() as u16;
        assert_eq!(line(footer.x, footer.x + hint_width), hint);
        for i in 0..hint_width {
            assert_eq!(
                buffer[(footer.x + i, row)].style().fg,
                Some(theme.muted_text),
                "hint cell {i} wears the footer colour"
            );
        }

        // The button zone: right-aligned, each button exactly its shared
        // button text, its bracketed key in the accent, its label in the
        // theme's foreground (T64.1), and never bold.
        let rects = footer_button_rects(footer, buttons);
        assert_eq!(rects.len(), buttons.len(), "one rect per button");
        assert_eq!(
            rects.last().expect("buttons exist").right(),
            footer.right(),
            "the buttons are right-aligned"
        );
        for ((key, label), rect) in buttons.iter().zip(&rects) {
            assert_eq!(line(rect.x, rect.right()), button_text(key, label));
            let accent_width =
                u16::try_from(format!(" [ {key} ] ").chars().count()).unwrap_or(rect.width);
            if !label.is_empty() {
                assert_ne!(
                    theme.foreground, theme.highlighted_text,
                    "the label colour differs from the accent"
                );
            }
            for i in 0..rect.width {
                let cell = &buffer[(rect.x + i, row)];
                let expected = if i < accent_width {
                    theme.highlighted_text
                } else {
                    theme.foreground
                };
                assert_eq!(
                    cell.style().fg,
                    Some(expected),
                    "button cell {i} wears its zone's colour"
                );
                assert!(
                    !cell.style().add_modifier.contains(Modifier::BOLD),
                    "buttons are never bold"
                );
            }
        }

        // The left zone carries no accent and no bracket, and two blank
        // columns separate it from the button zone.
        let left_end = rects[0].x;
        for x in footer.x + hint_width..left_end.saturating_sub(2) {
            let cell = &buffer[(x, row)];
            assert_ne!(
                cell.style().fg,
                Some(theme.highlighted_text),
                "no hint cell wears the accent at x={x}"
            );
            assert!(
                !matches!(cell.symbol(), "[" | "]"),
                "no bracket in the hint zone at x={x}"
            );
        }
        for x in left_end.saturating_sub(2)..left_end {
            assert_eq!(buffer[(x, row)].symbol(), " ", "a gap column at x={x}");
        }
    }

    /// The footer row as `symbol|fg|bg` cells, so the snapshot pins the two
    /// zones' colours and not just their symbols.
    fn styled_footer(app: &App, footer: ratatui::layout::Rect) -> String {
        let buffer = buffer(app);
        let spell = |colour: Color| match colour {
            Color::Reset => "-".to_string(),
            Color::Rgb(r, g, b) => format!("{r},{g},{b}"),
            Color::Indexed(i) => format!("i{i}"),
            other => format!("{other:?}"),
        };
        (footer.x..footer.right())
            .map(|x| {
                let cell = &buffer[(x, footer.y)];
                format!("{}|{}|{}", cell.symbol(), spell(cell.fg), spell(cell.bg))
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn dialog_bottom_line_two_zones() {
        let app = open_dialog();
        let buffer = buffer_wide(&app);
        assert_two_zones(
            &buffer,
            app.dialog_footer.get(),
            "↑↓←→ move · Shift-Enter new line · Ctrl+U clear",
            &[("Enter", app.primary_label()), ("Esc", "Close")],
            &app.theme(),
        );
        // The queue holds pending tasks and the input is empty: Start.
        assert_eq!(app.primary_label(), "Start");
    }

    #[test]
    fn dialog_bottom_line_two_zones_in_a_palette_theme() {
        let mut app = themed(app(), ThemeKey::TokyoNightDark);
        press(&mut app, KeyCode::Char('a'));
        let buffer = buffer_wide(&app);
        let theme = Theme::resolve(ThemeKey::TokyoNightDark, Some(true));
        assert_two_zones(
            &buffer,
            app.dialog_footer.get(),
            "↑↓←→ move · Shift-Enter new line · Ctrl+U clear",
            &[("Enter", app.primary_label()), ("Esc", "Close")],
            &theme,
        );
        assert_ne!(theme.highlighted_text, Theme::DARK.highlighted_text);
        assert_ne!(theme.muted_text, Theme::DARK.muted_text);
    }

    #[test]
    fn dialog_bottom_line_buttons_answer_clicks() {
        // Enter with text submits and closes the dialog.
        let mut app = open_dialog();
        type_text(&mut app, "add a login page");
        buffer(&app);
        let rects = footer_button_rects(
            app.dialog_footer.get(),
            &[("Enter", "Submit"), ("Esc", "Close")],
        );
        assert_eq!(
            app.on_mouse(click(rects[0].x + 2, rects[0].y)),
            Action::SubmitTasks("add a login page".into())
        );
        // The click takes the dialog exactly where the Enter key takes it: the
        // driver's accepted-submit flow is what closes it.
        app.on_tasks_submitted();
        assert!(!app.dialog_open);

        // Enter on an empty input keeps the dialog open with the inline refusal.
        let mut app = open_dialog();
        buffer(&app);
        let rects = footer_button_rects(
            app.dialog_footer.get(),
            &[("Enter", "Start"), ("Esc", "Close")],
        );
        assert_eq!(
            app.on_mouse(click(rects[0].x + 5, rects[0].y)),
            Action::None
        );
        assert!(app.dialog_open);
        assert!(
            app.dialog_status
                .as_deref()
                .unwrap_or_default()
                .contains("Nothing to submit")
        );

        // Esc closes and keeps the typed text.
        let mut app = open_dialog();
        type_text(&mut app, "hello");
        buffer(&app);
        let rects = footer_button_rects(
            app.dialog_footer.get(),
            &[("Enter", "Submit"), ("Esc", "Close")],
        );
        assert_eq!(
            app.on_mouse(click(rects[1].x + 2, rects[1].y)),
            Action::None
        );
        assert!(!app.dialog_open);
        assert_eq!(app.dialog_text, "hello");

        // Clicks on the hint zone and on the input area change nothing.
        let mut app = open_dialog();
        buffer(&app);
        let footer = app.dialog_footer.get();
        assert_eq!(app.on_mouse(click(footer.x + 1, footer.y)), Action::None);
        assert!(app.dialog_open);
        assert_eq!(
            app.on_mouse(click(footer.x + 1, footer.y - 5)),
            Action::None
        );
        assert!(app.dialog_open);
    }

    #[test]
    fn stop_bottom_line_two_zones() {
        let app = open_stop();
        let buffer = buffer(&app);
        assert_two_zones(
            &buffer,
            app.stop_footer.get(),
            "↑↓ move",
            &[("Enter", "Confirm"), ("Esc", "Close")],
            &app.theme(),
        );
    }

    #[test]
    fn stop_bottom_line_two_zones_in_a_palette_theme() {
        let mut app = themed(app(), ThemeKey::TokyoNightDark);
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        press(&mut app, KeyCode::Esc);
        assert!(app.stop_open);
        let buffer = buffer(&app);
        assert_two_zones(
            &buffer,
            app.stop_footer.get(),
            "↑↓ move",
            &[("Enter", "Confirm"), ("Esc", "Close")],
            &Theme::resolve(ThemeKey::TokyoNightDark, Some(true)),
        );
    }

    #[test]
    fn stop_bottom_line_buttons_answer_clicks() {
        // Enter on Soft stop soft-stops the build.
        let mut app = open_stop();
        buffer(&app);
        let rects = footer_button_rects(
            app.stop_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Close")],
        );
        assert_eq!(
            app.on_mouse(click(rects[0].x + 3, rects[0].y)),
            Action::Quit
        );
        assert!(!app.stop_open);
        assert!(app.stopping);

        // Enter on Interrupt interrupts.
        let mut app = open_stop();
        press(&mut app, KeyCode::Down);
        buffer(&app);
        let rects = footer_button_rects(
            app.stop_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Close")],
        );
        assert_eq!(
            app.on_mouse(click(rects[0].x + 3, rects[0].y)),
            Action::Interrupt
        );
        assert!(!app.stop_open);
        assert!(app.stopping);

        // Enter on the third row keeps the build going.
        let mut app = open_stop();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        buffer(&app);
        let rects = footer_button_rects(
            app.stop_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Close")],
        );
        assert_eq!(
            app.on_mouse(click(rects[0].x + 3, rects[0].y)),
            Action::None
        );
        assert!(!app.stop_open);
        assert!(!app.stopping);
        assert_eq!(app.phase, Phase::Running);

        // Esc closes with no effect: the build continues.
        let mut app = open_stop();
        buffer(&app);
        let rects = footer_button_rects(
            app.stop_footer.get(),
            &[("Enter", "Confirm"), ("Esc", "Close")],
        );
        assert_eq!(
            app.on_mouse(click(rects[1].x + 2, rects[1].y)),
            Action::None
        );
        assert!(!app.stop_open);
        assert!(!app.stopping);
        assert_eq!(app.phase, Phase::Running);
        assert_eq!(app.current_task.as_deref(), Some("T1.2"));

        // Clicks on the hint zone and on the choice rows change nothing.
        let mut app = open_stop();
        buffer(&app);
        let footer = app.stop_footer.get();
        assert_eq!(app.on_mouse(click(footer.x + 1, footer.y)), Action::None);
        assert!(app.stop_open);
        assert_eq!(
            app.on_mouse(click(footer.x + 2, footer.y - 4)),
            Action::None
        );
        assert!(app.stop_open);
        assert_eq!(app.stop_selected, 0);
    }

    /// The styled footer rows in the default and a palette theme, so the
    /// accent-coloured buttons and the muted hints are pinned cell by cell.
    #[test]
    fn dialog_footer_row_styled_dark_theme() {
        let app = open_dialog();
        buffer(&app);
        insta::assert_snapshot!(styled_footer(&app, app.dialog_footer.get()));
    }

    #[test]
    fn dialog_footer_row_styled_palette_theme() {
        let mut app = themed(app(), ThemeKey::TokyoNightDark);
        press(&mut app, KeyCode::Char('a'));
        buffer(&app);
        insta::assert_snapshot!(styled_footer(&app, app.dialog_footer.get()));
    }

    #[test]
    fn stop_footer_row_styled_dark_theme() {
        let app = open_stop();
        buffer(&app);
        insta::assert_snapshot!(styled_footer(&app, app.stop_footer.get()));
    }

    #[test]
    fn stop_footer_row_styled_palette_theme() {
        let mut app = themed(app(), ThemeKey::TokyoNightDark);
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        press(&mut app, KeyCode::Esc);
        buffer(&app);
        insta::assert_snapshot!(styled_footer(&app, app.stop_footer.get()));
    }
}

/// Every modal's top-right close button (T66.1): the shared ` [ x ] ` drawn
/// in the theme's button accent on the modal's title row -- inside the modal,
/// clear of its title, down to the modal's minimum width -- and a click on
/// its rectangle runs exactly the modal's Esc key: the add-task dialog closes
/// keeping its text and the stop dialog closes leaving the build unchanged,
/// in every theme, while Esc itself behaves exactly as before.
mod close_button {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use patok_core::config::Theme as ThemeKey;
    use patok_tui::{
        Action, CLOSE_BUTTON_WIDTH, Theme, button_text, close_button_rect, dialog_area, stop_area,
    };
    use ratatui::layout::{Position, Rect};
    use ratatui::style::Color;

    const W: u16 = 80;
    const H: u16 = 24;

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn buffer(app: &App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn open_dialog() -> App {
        let mut app = app();
        press(&mut app, KeyCode::Char('a'));
        assert!(app.dialog_open);
        app
    }

    fn open_stop() -> App {
        let mut app = app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        app.apply(EngineEvent::TaskStarted {
            id: "T1.2".into(),
            description: "add the parser for task files".into(),
        });
        press(&mut app, KeyCode::Esc);
        assert!(app.stop_open);
        app
    }

    fn themed(mut app: App, theme: ThemeKey) -> App {
        app.tui.theme = theme;
        app.tui.truecolor = Some(true);
        app
    }

    /// Asserts one modal's title row: the close button sits flush inside the
    /// modal's top-right corner, spells the shared button text with every
    /// cell in the given theme's button accent, and the modal's title text
    /// survives intact to its left. Returns the button's rect.
    fn assert_close_button(
        buffer: &ratatui::buffer::Buffer,
        modal: Rect,
        title: &str,
        theme: &Theme,
    ) -> Rect {
        let close = close_button_rect(modal);
        assert_ne!(close, Rect::default(), "the modal fits the button");
        assert_eq!(close.y, modal.y, "on the title row");
        assert_eq!(
            close.right(),
            modal.right() - 1,
            "one column short of the top-right corner"
        );
        assert!(modal.contains(Position::new(close.x, close.y)));
        assert!(modal.contains(Position::new(close.right() - 1, close.y)));
        let line = |from: u16, to: u16| -> String {
            (from..to).map(|x| buffer[(x, close.y)].symbol()).collect()
        };
        assert_eq!(line(close.x, close.right()), button_text("x", ""));
        for i in 0..close.width {
            assert_eq!(
                buffer[(close.x + i, close.y)].style().fg,
                Some(theme.highlighted_text),
                "button cell {i} wears the accent"
            );
        }
        let title_row = line(modal.x + 1, close.x);
        assert!(
            title_row.contains(title),
            "title {title:?} intact in {title_row:?}"
        );
        close
    }

    /// The close button row as `symbol|fg|bg` cells, so the snapshot pins the
    /// accent-coloured button and not just its symbols.
    fn styled_close_row(buffer: &ratatui::buffer::Buffer, close: Rect) -> String {
        let spell = |colour: Color| match colour {
            Color::Reset => "-".to_string(),
            Color::Rgb(r, g, b) => format!("{r},{g},{b}"),
            Color::Indexed(i) => format!("i{i}"),
            other => format!("{other:?}"),
        };
        (close.x..close.right())
            .map(|x| {
                let cell = &buffer[(x, close.y)];
                format!("{}|{}|{}", cell.symbol(), spell(cell.fg), spell(cell.bg))
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn close_button_rect_sits_top_right_and_is_zero_when_too_narrow() {
        let modal = Rect::new(10, 5, 60, 20);
        let close = close_button_rect(modal);
        assert_eq!(
            close,
            Rect::new(
                modal.right() - 1 - CLOSE_BUTTON_WIDTH,
                modal.y,
                CLOSE_BUTTON_WIDTH,
                1
            )
        );
        assert!(modal.contains(Position::new(close.x, close.y)));
        assert!(modal.contains(Position::new(close.right() - 1, close.y)));
        // A modal too narrow to hold the button inside its borders records no
        // rect: nothing renders and no real position falls inside a zero rect.
        assert_eq!(
            close_button_rect(Rect::new(0, 0, CLOSE_BUTTON_WIDTH + 1, 9)),
            Rect::default()
        );
        assert_ne!(
            close_button_rect(Rect::new(0, 0, CLOSE_BUTTON_WIDTH + 2, 9)),
            Rect::default()
        );
    }

    #[test]
    fn dialog_shows_the_close_button_top_right() {
        let app = open_dialog();
        let buffer = buffer(&app, W, H);
        let modal = dialog_area(Rect::new(0, 0, W, H));
        assert_close_button(&buffer, modal, "What do you want to do?", &app.theme());
        // The renderer records exactly the rect the hit-test computes.
        assert_eq!(app.dialog_close.get(), close_button_rect(modal));
    }

    #[test]
    fn dialog_close_button_recolours_with_the_theme() {
        let app = themed(open_dialog(), ThemeKey::TokyoNightDark);
        let buffer = buffer(&app, W, H);
        let modal = dialog_area(Rect::new(0, 0, W, H));
        let theme = Theme::resolve(ThemeKey::TokyoNightDark, Some(true));
        assert_close_button(&buffer, modal, "What do you want to do?", &theme);
        assert_ne!(theme.highlighted_text, Theme::DARK.highlighted_text);
    }

    #[test]
    fn stop_dialog_shows_the_close_button_top_right() {
        let app = open_stop();
        let buffer = buffer(&app, W, H);
        let modal = stop_area(Rect::new(0, 0, W, H));
        assert_close_button(&buffer, modal, "Stop build", &app.theme());
        assert_eq!(app.stop_close.get(), close_button_rect(modal));
    }

    /// Both modals keep their title and button apart at their minimum width:
    /// the dialog at 60 columns, the stop dialog at 54 (the smallest screens
    /// clamp the modal to the full screen).
    #[test]
    fn close_button_fits_the_modals_at_their_minimum_widths() {
        let app = open_dialog();
        let narrow = buffer(&app, 60, 20);
        assert_close_button(
            &narrow,
            Rect::new(0, 0, 60, 20),
            "What do you want to do?",
            &app.theme(),
        );

        let app = open_stop();
        let narrow = buffer(&app, 54, 9);
        assert_close_button(&narrow, Rect::new(0, 0, 54, 9), "Stop build", &app.theme());
    }

    #[test]
    fn dialog_close_button_click_runs_esc() {
        // A click on the button closes the dialog keeping the typed text.
        let mut app = open_dialog();
        type_text(&mut app, "add a login page");
        buffer(&app, W, H);
        let close = app.dialog_close.get();
        assert_eq!(app.on_mouse(click(close.x + 3, close.y)), Action::None);
        assert!(!app.dialog_open);
        assert_eq!(app.dialog_text, "add a login page");

        // Esc takes exactly the same path with the same outcome.
        let mut app = open_dialog();
        type_text(&mut app, "add a login page");
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(!app.dialog_open);
        assert_eq!(app.dialog_text, "add a login page");

        // Clicks on the title row outside the button and on the modal's body
        // change nothing.
        let mut app = open_dialog();
        buffer(&app, W, H);
        let close = app.dialog_close.get();
        assert_eq!(app.on_mouse(click(close.x - 1, close.y)), Action::None);
        assert!(app.dialog_open);
        assert_eq!(app.on_mouse(click(close.x + 3, close.y + 5)), Action::None);
        assert!(app.dialog_open);
    }

    #[test]
    fn stop_close_button_click_runs_esc() {
        // A click closes the dialog leaving the build unchanged.
        let mut app = open_stop();
        buffer(&app, W, H);
        let close = app.stop_close.get();
        assert_eq!(app.on_mouse(click(close.x + 3, close.y)), Action::None);
        assert!(!app.stop_open);
        assert!(!app.stopping);
        assert_eq!(app.phase, Phase::Running);
        assert_eq!(app.current_task.as_deref(), Some("T1.2"));

        // Esc closes it the same way.
        let mut app = open_stop();
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(!app.stop_open);
        assert!(!app.stopping);
        assert_eq!(app.current_task.as_deref(), Some("T1.2"));

        // A click on the choices leaves the dialog open and the selection put.
        let mut app = open_stop();
        press(&mut app, KeyCode::Down);
        buffer(&app, W, H);
        let close = app.stop_close.get();
        assert_eq!(app.on_mouse(click(close.x - 2, close.y + 4)), Action::None);
        assert!(app.stop_open);
        assert_eq!(app.stop_selected, 1);
    }

    /// The dialog's styled close-button row in the default and a palette
    /// theme, so the accent-coloured ` [ x ] ` is pinned cell by cell in both.
    #[test]
    fn dialog_close_button_row_styled_dark_theme() {
        let app = open_dialog();
        let buffer = buffer(&app, W, H);
        insta::assert_snapshot!(styled_close_row(
            &buffer,
            close_button_rect(dialog_area(Rect::new(0, 0, W, H)))
        ));
    }

    #[test]
    fn dialog_close_button_row_styled_palette_theme() {
        let mut app = themed(app(), ThemeKey::TokyoNightDark);
        press(&mut app, KeyCode::Char('a'));
        let buffer = buffer(&app, W, H);
        insta::assert_snapshot!(styled_close_row(
            &buffer,
            close_button_rect(dialog_area(Rect::new(0, 0, W, H)))
        ));
    }
}

/// Cell-level rendering for the theme snapshots: every cell as
/// `symbol|foreground|background`, so a snapshot captures the colours, not
/// just the layout.
mod theme {
    use super::{App, Terminal, draw};
    use patok_core::config::Theme as ThemeKey;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    const W: u16 = 80;
    const H: u16 = 24;

    fn draw_styled(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
        terminal.draw(|frame| super::render(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let spell = |colour: Color| match colour {
            Color::Reset => "-".to_string(),
            Color::Rgb(r, g, b) => format!("{r},{g},{b}"),
            Color::Indexed(i) => format!("i{i}"),
            other => format!("{other:?}"),
        };
        (0..H)
            .map(|y| {
                (0..W)
                    .map(|x| {
                        let cell = &buffer[(x, y)];
                        format!("{}|{}|{}", cell.symbol(), spell(cell.fg), spell(cell.bg))
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn themed(theme: ThemeKey) -> App {
        let mut app = super::app();
        app.tui.theme = theme;
        app.tui.truecolor = Some(true);
        app
    }

    /// The dark and light variants keep the exact same symbol layout -- the
    /// theme recolours the shell without touching its structure.
    #[test]
    fn dark_and_light_themes_keep_the_same_layout() {
        let dark = draw(&themed(ThemeKey::TokyoNightDark), W, H);
        let light = draw(&themed(ThemeKey::CatppuccinLatte), W, H);
        assert_eq!(dark, light);
        // And both restyle the whole frame: the theme's background shows up and
        // no cell keeps the terminal-default style.
        for (theme, background) in [
            (ThemeKey::TokyoNightDark, "26,27,38"),
            (ThemeKey::CatppuccinLatte, "239,241,245"),
        ] {
            let styled = draw_styled(&themed(theme));
            assert!(
                styled.contains(&format!("|{background}")),
                "the {theme:?} frame background never appears"
            );
            assert!(
                !styled.contains("|-|-"),
                "a cell kept the terminal-default style under {theme:?}"
            );
        }
    }

    #[test]
    fn tokyo_night_dark_recolours_the_shell() {
        insta::assert_snapshot!(draw_styled(&themed(ThemeKey::TokyoNightDark)));
    }

    #[test]
    fn catppuccin_latte_recolours_the_shell() {
        insta::assert_snapshot!(draw_styled(&themed(ThemeKey::CatppuccinLatte)));
    }
}

/// The pipeline rail (T50.1, T53.1): the one rendering of the engine's
/// pipeline state, vertical in every engine state, its colours through a
/// plan-then-build run, the standalone tiles' flags and the frame behaviour
/// around the fixed column.
mod pipeline {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use patok_core::config::RailMode;
    use patok_core::pipeline::{Stage, Tile, TileStatus};
    use patok_tui::Theme;
    use patok_tui::{
        Action, FrameFocus, RAIL_WIDTH, TileId, rail_connector_rects, rail_tile_rects, rail_width,
    };
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier};

    /// Today's stages with the given statuses and muted standalone tiles.
    fn stage_state(plan: TileStatus, build: TileStatus) -> PipelineState {
        PipelineState {
            stages: vec![
                Tile {
                    stage: Stage::Plan,
                    status: plan,
                },
                Tile {
                    stage: Stage::Build,
                    status: build,
                },
            ],
            ..PipelineState::today()
        }
    }

    /// The full stage rail (research, plan, build, review) with the given
    /// statuses.
    fn full_stage_state(
        research: TileStatus,
        plan: TileStatus,
        build: TileStatus,
        review: TileStatus,
    ) -> PipelineState {
        PipelineState {
            stages: vec![
                Tile {
                    stage: Stage::Research,
                    status: research,
                },
                Tile {
                    stage: Stage::Plan,
                    status: plan,
                },
                Tile {
                    stage: Stage::Build,
                    status: build,
                },
                Tile {
                    stage: Stage::Review,
                    status: review,
                },
            ],
            ..PipelineState::today()
        }
    }

    /// The research stage runs before the plan stage (T68.1): the RESEARCH tile
    /// is accent and bold while the session runs, then green once the plan
    /// stage takes over, with the stages after it still muted.
    #[test]
    fn the_research_tile_lights_up_active_then_done() {
        let theme = Theme::DARK;
        let mut app = running_app();
        app.tui.rail_mode = RailMode::Compact;

        app.apply(EngineEvent::PipelineChanged {
            state: full_stage_state(
                TileStatus::Active,
                TileStatus::Pending,
                TileStatus::Pending,
                TileStatus::Pending,
            ),
        });
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Research)),
            ("[ R ]".to_string(), Some(theme.accent), true)
        );
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Plan)),
            ("[ P ]".to_string(), Some(theme.muted_text), false)
        );

        app.apply(EngineEvent::PipelineChanged {
            state: full_stage_state(
                TileStatus::Done,
                TileStatus::Active,
                TileStatus::Pending,
                TileStatus::Pending,
            ),
        });
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Research)),
            ("[ R ]".to_string(), Some(theme.success), false)
        );
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Plan)),
            ("[ P ]".to_string(), Some(theme.accent), true)
        );
    }

    /// A running build: the phase flips, a task starts and the focus moves to
    /// the agent output frame, so the shell draws the vertical rail.
    fn running_app() -> App {
        let mut app = app();
        app.apply(EngineEvent::PhaseChanged {
            phase: Phase::Running,
        });
        app.apply(EngineEvent::TaskStarted {
            id: "T1.2".into(),
            description: "add the parser for task files".into(),
        });
        app
    }

    /// A moved-pointer event at (column, row).
    fn moved(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Moved,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// A left-click event at (column, row).
    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// A wheel event at (column, row).
    fn wheel(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// The tile's drawn cells at its rect: the trimmed text, the foreground
    /// colour and whether any cell is bold.
    fn tile_cells(buffer: &ratatui::buffer::Buffer, rect: Rect) -> (String, Option<Color>, bool) {
        let mut text = String::new();
        let mut fg = None;
        let mut bold = false;
        for x in rect.x..rect.right() {
            let cell = &buffer[(x, rect.y)];
            text.push_str(cell.symbol());
            fg = fg.or(cell.style().fg);
            bold = bold || cell.style().add_modifier.contains(Modifier::BOLD);
        }
        (text.trim().to_string(), fg, bold)
    }

    /// The tile's rect in the last-rendered rail, reading the same rect math
    /// the renderer draws through.
    fn tile_rect(app: &App, id: TileId) -> Rect {
        let area = app.pipeline_area.get();
        rail_tile_rects(&app.pipeline, area, app.tui.rail_mode)
            .iter()
            .find(|(tile, _)| *tile == id)
            .map(|(_, rect)| *rect)
            .expect("the tile renders")
    }

    /// 20 done tasks then 20 pending ones, so the task list has room to scroll.
    fn long_queue() -> String {
        let mut text = String::new();
        for n in 1..=20 {
            text.push_str(&format!("- [x] T9.{n}: done thing {n}\n"));
        }
        for n in 21..=40 {
            text.push_str(&format!("- [ ] T9.{n}: later thing {n}\n"));
        }
        text
    }

    /// The tiles and their colours follow the pipeline state through a
    /// plan-then-build run: the plan stage accent and bold while it runs, then
    /// green once the build stage takes over.
    #[test]
    fn the_tiles_follow_the_pipeline_state_through_a_plan_then_build_run() {
        let theme = Theme::DARK;
        let mut app = running_app();
        app.tui.rail_mode = RailMode::Compact;

        // The plan stage runs: Plan is accent and bold, the stage after it
        // is muted.
        app.apply(EngineEvent::PipelineChanged {
            state: stage_state(TileStatus::Active, TileStatus::Pending),
        });
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Plan)),
            ("[ P ]".to_string(), Some(theme.accent), true)
        );
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Build)),
            ("[ B ]".to_string(), Some(theme.muted_text), false)
        );

        // The build stage runs: Plan went green, Build is accent and bold.
        app.apply(EngineEvent::PipelineChanged {
            state: stage_state(TileStatus::Done, TileStatus::Active),
        });
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Plan)),
            ("[ P ]".to_string(), Some(theme.success), false)
        );
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Build)),
            ("[ B ]".to_string(), Some(theme.accent), true)
        );
    }

    /// The review tile lights around the reviewer's session (T70.1): pending
    /// while the earlier stages run, accent and bold while the reviewer
    /// reviews, green once the review is done -- and a skipped review goes
    /// straight to green, its dash living in the task-line indicator.
    #[test]
    fn the_review_tile_is_active_while_the_reviewer_runs_then_done() {
        let theme = Theme::DARK;
        let mut app = running_app();
        app.tui.rail_mode = RailMode::Compact;

        // The builder runs: the review tile is still pending.
        app.apply(EngineEvent::PipelineChanged {
            state: full_stage_state(
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Active,
                TileStatus::Pending,
            ),
        });
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Review)),
            ("[ RV ]".to_string(), Some(theme.muted_text), false)
        );

        // The reviewer runs: the review tile is accent and bold.
        app.apply(EngineEvent::PipelineChanged {
            state: full_stage_state(
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Active,
            ),
        });
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Review)),
            ("[ RV ]".to_string(), Some(theme.accent), true)
        );

        // The review is done: green, like a skipped review's tile.
        app.apply(EngineEvent::PipelineChanged {
            state: full_stage_state(
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Done,
                TileStatus::Done,
            ),
        });
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Review)),
            ("[ RV ]".to_string(), Some(theme.success), false)
        );
    }

    /// The DI tile reflects its flag: DISCOVER muted in sprint
    /// mode, accent while a round runs and green after one ran.
    #[test]
    fn the_standalone_tiles_reflect_their_flags() {
        let theme = Theme::DARK;

        // Sprint mode, engine idle: DI renders muted in the rail.
        let mut app = app();
        app.tui.rail_mode = RailMode::Compact;
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Discover)),
            ("[ DI ]".to_string(), Some(theme.muted_text), false)
        );

        // A discovery round runs: DI is accent and bold in the rail.
        app.apply(EngineEvent::DiscoveryChanged { discovering: true });
        app.apply(EngineEvent::PipelineChanged {
            state: PipelineState {
                discover: TileStatus::Active,
                ..stage_state(TileStatus::Muted, TileStatus::Muted)
            },
        });
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Discover)),
            ("[ DI ]".to_string(), Some(theme.accent), true)
        );

        // The round ran in this session: DI turns green.
        app.apply(EngineEvent::DiscoveryChanged { discovering: false });
        app.apply(EngineEvent::PipelineChanged {
            state: PipelineState {
                discover: TileStatus::Done,
                ..stage_state(TileStatus::Muted, TileStatus::Muted)
            },
        });
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Discover)),
            ("[ DI ]".to_string(), Some(theme.success), false)
        );
    }

    /// A pointer move over a rail tile is inert (T87.1): the hover action is
    /// gone, so nothing changes -- no tile name lands in the status line, and
    /// neither the frame focus nor either scroll moves.
    #[test]
    fn a_pointer_move_over_the_rail_changes_nothing() {
        let mut shell = running_app();
        shell.tui.rail_mode = RailMode::Compact;
        shell.apply(EngineEvent::PipelineChanged {
            state: stage_state(TileStatus::Active, TileStatus::Pending),
        });
        draw_buffer(&shell);

        // The pointer crosses the plan and discover tiles: no action, no focus
        // or scroll change, and the status line keeps its key hints.
        for id in [TileId::Plan, TileId::Discover] {
            let rect = tile_rect(&shell, id);
            assert_eq!(shell.on_mouse(moved(rect.x + 2, rect.y)), Action::None);
        }
        assert_eq!(shell.focus, FrameFocus::Output);
        assert_eq!((shell.scroll, shell.task_scroll), (0, 0));
        let hints = draw(&shell, 130, 24).lines().last().unwrap().to_string();
        assert!(hints.contains(" menu"), "{hints}");
        assert!(!hints.contains(" stop build "), "{hints}");
        assert!(!hints.trim_end().ends_with("Plan"), "{hints}");
        assert!(!hints.trim_end().ends_with("Discover"), "{hints}");

        // The same holds over the normal-mode boxes and while idle.
        let mut idle = app();
        idle.tui.rail_mode = RailMode::Normal;
        draw_buffer(&idle);
        let rect = tile_rect(&idle, TileId::Build);
        assert_eq!(idle.on_mouse(moved(rect.x + 6, rect.y)), Action::None);
        let status = draw(&idle, 130, 24).lines().last().unwrap().to_string();
        assert!(status.starts_with(" STOPPED  sprint"), "{status}");
        assert!(!status.trim_end().ends_with("Build"), "{status}");
    }

    #[test]
    fn detailed_mode_displays_configured_agent_provider_and_model_and_ignores_v() {
        let mut shell = app();
        shell.settings.insert(
            "provider".into(),
            patok_core::config::SettingValue::Str("claude".into()),
        );
        shell.settings.insert(
            "planner_model".into(),
            patok_core::config::SettingValue::Str("sonnet-xl".into()),
        );
        shell.tui.rail_mode = RailMode::Detailed;
        let screen = draw(&shell, 100, 30);
        assert!(screen.contains("claude"), "{screen}");
        assert!(screen.contains("sonnet-xl"), "{screen}");
        assert_eq!(rail_width(RailMode::Detailed), rail_width(RailMode::Normal));
        // T125.1 removed the `v` rail-view binding: a bare `v` is a no-op
        // in the shell and the rail mode only changes through the settings
        // overlay.
        assert_eq!(
            shell.on_key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('v'),
                KeyModifiers::NONE
            )),
            Action::None
        );
        assert_eq!(shell.tui.rail_mode, RailMode::Detailed);
        assert!(
            rail_tile_rects(
                &shell.pipeline,
                Rect::new(0, 0, rail_width(RailMode::Detailed), 8),
                RailMode::Detailed
            )
            .len()
                < shell.pipeline.stages.len()
        );
    }

    /// The rail is the one rendering in every engine state (T53.1): the idle,
    /// planning, running and discovery shells all draw the same narrow
    /// vertical rail column left of the frames -- the same tiles, letters,
    /// layout and colours -- so nothing about it changes when the engine
    /// moves between idle and running.
    #[test]
    fn the_rail_is_vertical_and_identical_in_every_engine_state() {
        let theme = Theme::DARK;
        let state = PipelineState {
            discover: TileStatus::Done,
            ..stage_state(TileStatus::Done, TileStatus::Active)
        };
        let mut planning = app();
        planning.apply(EngineEvent::PlanningChanged { planning: true });
        let mut discovery = app();
        discovery.apply(EngineEvent::DiscoveryChanged { discovering: true });
        let mut shells: Vec<(&str, App)> = vec![
            ("idle", app()),
            ("planning", planning),
            ("running", running_app()),
            ("discovery", discovery),
        ];
        for (_, shell) in &mut shells {
            shell.pipeline = state.clone();
            shell.tui.rail_mode = RailMode::Compact;
        }

        let ids = [TileId::Plan, TileId::Build, TileId::Discover];
        // The letters, colours and boldness the state implies, shared by
        // every shell: Plan done, Build active, DI done.
        let expected: Vec<(String, Option<Color>, bool)> = vec![
            ("[ P ]".to_string(), Some(theme.success), false),
            ("[ B ]".to_string(), Some(theme.accent), true),
            ("[ DI ]".to_string(), Some(theme.success), false),
        ];

        // The whole rail area's drawn cells, as the equality reference:
        // symbol, foreground, background and modifiers.
        let column = |buffer: &ratatui::buffer::Buffer, area: Rect| {
            (area.y..area.bottom())
                .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let cell = &buffer[(x, y)];
                    (cell.symbol().to_string(), cell.style())
                })
                .collect::<Vec<_>>()
        };
        let reference = column(&draw_buffer(&shells[0].1), shells[0].1.pipeline_area.get());

        for (name, shell) in &shells {
            let buffer = draw_buffer(shell);
            // The frames shrink to the width left of the rail's column.
            assert_eq!(shell.output_area.get().x, RAIL_WIDTH, "{name}");
            assert_eq!(shell.tasks_area.get().x, RAIL_WIDTH, "{name}");
            // The rail is vertical: every tile sits inside the fixed
            // left column, one row per tile, in the pipeline's order.
            let rects = rail_tile_rects(
                &shell.pipeline,
                shell.pipeline_area.get(),
                RailMode::Compact,
            );
            assert_eq!(
                rects.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                ids.to_vec(),
                "{name}"
            );
            let rows: Vec<u16> = rects.iter().map(|(_, rect)| rect.y).collect();
            let mut stacked = rows.clone();
            stacked.sort_unstable();
            stacked.dedup();
            assert_eq!(rows, stacked, "{name} stacks its tiles one per row");
            for ((_, rect), want) in rects.iter().zip(&expected) {
                assert!(rect.x < RAIL_WIDTH, "{name}: a tile leaves the column");
                assert!(
                    rect.right() <= RAIL_WIDTH,
                    "{name}: a tile leaves the column"
                );
                assert_eq!(rect.height, 1);
                assert_eq!(&tile_cells(&buffer, *rect), want, "{name}");
            }
            // The down-arrow connector lines the tiles up under the
            // Plan tile's letter.
            assert_eq!(
                rail_connector_rects(
                    &shell.pipeline,
                    shell.pipeline_area.get(),
                    RailMode::Compact
                ),
                vec![Rect::new(RAIL_WIDTH / 2, rects[0].1.y + 1, 1, 1)],
                "{name}"
            );
            // And the whole rail area draws identically to the idle shell's.
            assert_eq!(
                column(&buffer, shell.pipeline_area.get()),
                reference,
                "{name} rail differs"
            );
        }
    }

    /// The rail opens with one purely blank row above its first tile (T63.1):
    /// nothing is drawn in the rail column's top row -- no border,
    /// connector, tile or colour -- and every tile and connector sits
    /// exactly one row below the rail area's top, drawing the same text and
    /// colours as before, in both rail modes and every engine state, while
    /// the frames keep their layout (the merged view is the one face of both
    /// the main view and the Explore tab, T53.1).
    #[test]
    fn the_row_above_the_first_tile_is_blank_in_both_modes_and_every_state() {
        let theme = Theme::DARK;
        let state = PipelineState {
            discover: TileStatus::Done,
            ..stage_state(TileStatus::Done, TileStatus::Active)
        };

        for mode in [RailMode::Compact, RailMode::Normal] {
            let mut planning = app();
            planning.apply(EngineEvent::PlanningChanged { planning: true });
            let mut discovery = app();
            discovery.apply(EngineEvent::DiscoveryChanged { discovering: true });
            let mut shells: Vec<(&str, App)> = vec![
                ("idle", app()),
                ("planning", planning),
                ("running", running_app()),
                ("discovery", discovery),
            ];
            for (_, shell) in &mut shells {
                shell.pipeline = state.clone();
                shell.tui.rail_mode = mode;
            }
            let width = rail_width(mode);
            // The tiles' text, colours and boldness, the same per status in
            // both modes: Plan done, Build active, DI done.
            let expected: Vec<(String, Option<Color>, bool)> = match mode {
                RailMode::Compact => vec![
                    ("[ P ]".to_string(), Some(theme.success), false),
                    ("[ B ]".to_string(), Some(theme.accent), true),
                    ("[ DI ]".to_string(), Some(theme.success), false),
                ],
                RailMode::Normal | RailMode::Detailed => vec![
                    ("[   Plan   ]".to_string(), Some(theme.success), false),
                    ("[  Build   ]".to_string(), Some(theme.accent), true),
                    ("[ Discover ]".to_string(), Some(theme.success), false),
                ],
            };

            for (name, shell) in &shells {
                let buffer = draw_buffer(shell);
                let area = shell.pipeline_area.get();
                // The frames keep their layout: right of the rail's fixed
                // column, starting on the body's first row (T86.1).
                assert_eq!(area.width, width, "{name}");
                assert_eq!(shell.output_area.get().x, width, "{name}");
                assert_eq!(shell.tasks_area.get().x, width, "{name}");
                assert_eq!(shell.output_area.get().y, 0, "{name}");

                // The rail column's top row is purely empty: blank symbols
                // carrying nothing but the base style, so no border,
                // connector, tile or colour was drawn in it.
                for x in area.x..area.right() {
                    let cell = &buffer[(x, area.y)];
                    assert_eq!(cell.symbol(), " ", "{name}: the blank row draws");
                    assert_eq!(
                        (cell.style().fg, cell.style().bg),
                        (Some(Color::Reset), Some(Color::Reset)),
                        "{name}: the blank row carries a colour"
                    );
                }

                // Every tile shifted down by exactly one row: the first tile
                // sits on the second row of the rail area, and the tiles
                // below keep their old relative offsets.
                let rects = rail_tile_rects(&shell.pipeline, area, mode);
                let rows: Vec<u16> = rects.iter().map(|(_, rect)| rect.y).collect();
                assert_eq!(rows, vec![area.y + 1, area.y + 3, area.y + 5], "{name}");
                // The connector moved with the tiles: still exactly one row
                // below the first stage tile.
                assert_eq!(
                    rail_connector_rects(&shell.pipeline, area, mode),
                    vec![Rect::new(width / 2, area.y + 2, 1, 1)],
                    "{name}"
                );
                // And the tiles below the blank row are unchanged apart from
                // the shift: the same text, colours and boldness.
                for ((_, rect), want) in rects.iter().zip(&expected) {
                    assert_eq!(&tile_cells(&buffer, *rect), want, "{name}");
                }
            }
        }
    }

    /// The rail column never steals the frame focus or a scroll: the wheel
    /// over it changes nothing, the wheel over the narrower task list frame
    /// still scrolls it, and a click inside a frame still focuses it.
    #[test]
    fn the_rail_column_keeps_the_frame_focus_and_wheel_behaviour() {
        let mut app = running_app();
        app.tui.rail_mode = RailMode::Compact;
        app.apply(EngineEvent::TasksChanged {
            tasks: task::parse(&long_queue()),
        });
        for n in 1..=30 {
            app.apply(agent(AgentEvent::Text {
                text: format!("output line {n}"),
            }));
        }
        draw_buffer(&app);
        let output = app.output_area.get();
        let tasks = app.tasks_area.get();
        assert_eq!(app.focus, FrameFocus::Output);
        assert!(app.max_scroll.get() > 0);
        assert!(app.task_max_scroll.get() > 0);
        // The frames start right of the rail's fixed column.
        assert_eq!(output.x, 6);
        assert_eq!(tasks.x, 6);

        // A wheel step over the rail scrolls nothing and moves nothing.
        app.on_mouse(wheel(MouseEventKind::ScrollUp, 1, 2));
        assert_eq!((app.scroll, app.task_scroll), (0, 0));
        assert_eq!(app.focus, FrameFocus::Output);

        // A wheel step over the narrower task list frame still scrolls it.
        app.on_mouse(wheel(MouseEventKind::ScrollUp, tasks.x + 2, tasks.y + 3));
        assert_eq!((app.scroll, app.task_scroll), (0, 1));

        // A click inside a frame still focuses it.
        app.on_mouse(click(tasks.x + 2, tasks.y + 3));
        assert_eq!(app.focus, FrameFocus::Tasks);
    }

    /// The pipeline's four states as the unconfigured-default snapshots
    /// (T58.1): with no `rail_mode` key anywhere, the rail renders the
    /// normal-mode full-name boxes in the idle, planning, running and
    /// discovery states.
    #[test]
    fn idle_rail_with_every_tile_muted() {
        let app = app();
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    #[test]
    fn planning_rail() {
        let mut app = app();
        app.apply(EngineEvent::AgentChanged {
            agent: "planner".into(),
            started_ms: 0,
        });
        app.apply(EngineEvent::PlanningChanged { planning: true });
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    #[test]
    fn running_rail_mid_plan_then_build() {
        let mut app = running_app();
        app.apply(EngineEvent::PipelineChanged {
            state: stage_state(TileStatus::Active, TileStatus::Pending),
        });
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    #[test]
    fn discovery_rail_with_the_discover_tile_accent() {
        let mut app = app();
        app.apply(EngineEvent::DiscoveryChanged { discovering: true });
        app.apply(EngineEvent::PipelineChanged {
            state: PipelineState {
                discover: TileStatus::Active,
                ..stage_state(TileStatus::Muted, TileStatus::Muted)
            },
        });
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    /// Normal mode (T57.1): the rail renders the full bracketed names instead
    /// of the letter tiles, in every engine state -- the same one rendering
    /// the compact rail has -- with every box the width of the longest name
    /// plus its padding, the names centred, the arrows centred on the box
    /// column, the same colours and boldness per status, and the frames
    /// shrunk by the wider column.
    #[test]
    fn normal_mode_renders_full_name_boxes_in_every_engine_state() {
        let theme = Theme::DARK;
        let state = PipelineState {
            discover: TileStatus::Done,
            ..stage_state(TileStatus::Done, TileStatus::Active)
        };
        let mut planning = app();
        planning.apply(EngineEvent::PlanningChanged { planning: true });
        let mut discovery = app();
        discovery.apply(EngineEvent::DiscoveryChanged { discovering: true });
        let mut shells: Vec<(&str, App)> = vec![
            ("idle", app()),
            ("planning", planning),
            ("running", running_app()),
            ("discovery", discovery),
        ];
        for (_, shell) in &mut shells {
            shell.pipeline = state.clone();
            shell.tui.rail_mode = RailMode::Normal;
        }

        // The full names, colours and boldness the state implies, shared by
        // every shell: Plan done, Build active, DI done.
        let expected: Vec<(String, Option<Color>, bool)> = vec![
            ("[   Plan   ]".to_string(), Some(theme.success), false),
            ("[  Build   ]".to_string(), Some(theme.accent), true),
            ("[ Discover ]".to_string(), Some(theme.success), false),
        ];
        let width = rail_width(RailMode::Normal);
        assert_eq!(width, 12, "the longest name (Discover) plus its padding");

        // The whole rail area's drawn cells, as the cross-state equality
        // reference: symbol, foreground, background and modifiers.
        let column = |buffer: &ratatui::buffer::Buffer, area: Rect| {
            (area.y..area.bottom())
                .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let cell = &buffer[(x, y)];
                    (cell.symbol().to_string(), cell.style())
                })
                .collect::<Vec<_>>()
        };
        let reference = column(&draw_buffer(&shells[0].1), shells[0].1.pipeline_area.get());

        for (name, shell) in &shells {
            let buffer = draw_buffer(shell);
            // The frames shrink to what the wider normal-mode column leaves.
            assert_eq!(shell.output_area.get().x, width, "{name}");
            assert_eq!(shell.tasks_area.get().x, width, "{name}");
            let rects =
                rail_tile_rects(&shell.pipeline, shell.pipeline_area.get(), RailMode::Normal);
            assert_eq!(rects.len(), expected.len(), "{name}");
            for ((_, rect), want) in rects.iter().zip(&expected) {
                // Every box is equally wide and fills the column.
                assert_eq!(rect.width, width, "{name}");
                assert_eq!(rect.x, shell.pipeline_area.get().x, "{name}");
                assert_eq!(rect.height, 1);
                assert_eq!(&tile_cells(&buffer, *rect), want, "{name}");
            }
            // The connector arrow centres on the box column.
            assert_eq!(
                rail_connector_rects(&shell.pipeline, shell.pipeline_area.get(), RailMode::Normal),
                vec![Rect::new(width / 2, rects[0].1.y + 1, 1, 1)],
                "{name}"
            );
            // And the whole rail area draws identically to the idle shell's.
            assert_eq!(
                column(&buffer, shell.pipeline_area.get()),
                reference,
                "{name} rail differs"
            );
        }
    }

    /// Compact mode still draws the letter tiles (T57.1 keeps it unchanged):
    /// the same shells draw `[ P ]`-style tiles with the frames at the narrow
    /// column, and flipping the mode relayouts the very next frame with no
    /// engine round trip.
    #[test]
    fn switching_the_mode_relayouts_the_rail_and_frames_immediately() {
        let mut app = running_app();
        app.apply(EngineEvent::PipelineChanged {
            state: stage_state(TileStatus::Active, TileStatus::Pending),
        });
        app.tui.rail_mode = RailMode::Compact;

        // Compact: the letter tiles in the narrow column.
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Plan)),
            ("[ P ]".to_string(), Some(Theme::DARK.accent), true)
        );
        assert_eq!(app.pipeline_area.get().width, RAIL_WIDTH);
        assert_eq!(app.tasks_area.get().x, RAIL_WIDTH);
        let compact_width = app.tasks_area.get().width;

        // Normal: the full-name boxes in the wider column, on the next frame.
        app.tui.rail_mode = RailMode::Normal;
        let buffer = draw_buffer(&app);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&app, TileId::Plan)),
            ("[   Plan   ]".to_string(), Some(Theme::DARK.accent), true)
        );
        let width = rail_width(RailMode::Normal);
        assert_eq!(app.pipeline_area.get().width, width);
        assert_eq!(app.tasks_area.get().x, width);
        assert_eq!(
            compact_width - app.tasks_area.get().width,
            width - RAIL_WIDTH,
            "the frames lose exactly the wider column"
        );

        // And back to compact, with the layout restored.
        app.tui.rail_mode = RailMode::Compact;
        draw_buffer(&app);
        assert_eq!(app.pipeline_area.get().width, RAIL_WIDTH);
        assert_eq!(app.tasks_area.get().width, compact_width);
    }

    /// Switching the rail mode through the settings overlay (T57.1): with the
    /// key unset the row reads `normal` (the T58.1 default), and cycling the
    /// rail mode row drafts `compact` -- the save flow -- simulated exactly as
    /// the driver runs it, the tui-field path with no engine round trip --
    /// persists to the user-local `config.local.toml`, mirrors the value into
    /// the app, and relayouts the rail on the next frame: the explicit compact
    /// keeps showing the letter tiles.
    #[test]
    fn saving_a_rail_mode_change_from_the_overlay_persists_and_relouts() {
        use crossterm::event::{KeyCode, KeyEvent};
        use patok_core::config::{ConfigFiles, SettingValue};
        use patok_tui::{Action, Entry, ShellSettings};

        let dir = tempfile::tempdir().unwrap();
        let files = ConfigFiles {
            user_global: None,
            user_local: Some(dir.path().join("config.local.toml")),
        };
        let (mut shell_settings, warnings) = ShellSettings::with_files(files, dir.path(), None);
        assert!(warnings.is_empty());
        let mut shell = app();
        shell.tui = shell_settings.settings().clone();
        assert_eq!(shell.tui.rail_mode, RailMode::Normal);

        // Open the overlay and cycle the rail mode row: normal -> detailed -> compact.
        assert_eq!(
            shell.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
            Action::None
        );
        let index = shell
            .overlay
            .visible()
            .iter()
            .position(|entry| matches!(entry, Entry::Row(row) if row.key == "rail_mode"))
            .expect("the rail mode row is visible");
        shell.overlay.focus = index;
        assert_eq!(
            shell.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
            Action::None
        );
        assert_eq!(
            shell.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
            Action::None
        );
        assert_eq!(
            shell.overlay.drafts.get("rail_mode"),
            Some(&SettingValue::Str("compact".into()))
        );

        // Esc opens the three-choice dialog; Enter on Save asks the driver to
        // apply every drafted change.
        shell.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(shell.overlay.confirm_open);
        assert_eq!(
            shell.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::SaveSettings
        );

        // The driver's tui-field apply path, exactly as run.rs runs it: no
        // engine command, a write to the user-local layer, the mirrored value.
        for (schema, field, value) in shell.overlay.pending() {
            shell_settings.apply(&field, value.clone()).unwrap();
            shell.tui = shell_settings.settings().clone();
            shell.on_settings_applied(schema, &field, value);
        }
        shell.overlay.close();

        let text = std::fs::read_to_string(dir.path().join("config.local.toml")).unwrap();
        assert!(text.contains("rail_mode = \"compact\""), "{text}");
        assert_eq!(shell.tui.rail_mode, RailMode::Compact);

        // The rail relayouts immediately: the letter tiles back in the narrow
        // column, with the frames widened to match.
        let buffer = draw_buffer(&shell);
        assert_eq!(shell.pipeline_area.get().width, RAIL_WIDTH);
        assert_eq!(shell.tasks_area.get().x, RAIL_WIDTH);
        assert_eq!(
            tile_cells(&buffer, tile_rect(&shell, TileId::Plan)),
            ("[ P ]".to_string(), Some(Theme::DARK.muted_text), false)
        );
        assert!(draw(&shell, 80, 24).contains("[ P ]"));
    }

    /// The settings overlay shows `normal` as the rail mode's current value
    /// when the key is unset (T58.1): the default is visible, not just assumed.
    #[test]
    fn the_overlay_shows_normal_as_the_rail_mode_when_the_key_is_unset() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use patok_tui::{Action, Entry};

        let mut app = app();
        assert_eq!(app.tui.rail_mode, RailMode::Normal);
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
            Action::None
        );
        let index = app
            .overlay
            .visible()
            .iter()
            .position(|entry| matches!(entry, Entry::Row(row) if row.key == "rail_mode"))
            .expect("the rail mode row is visible");
        app.overlay.focus = index;
        let screen = draw(&app, 100, 30);
        assert!(screen.contains("rail mode  ‹ normal ›"), "{screen}");
    }

    /// Compact mode's four states as explicit snapshots (T57.1/T58.1): an
    /// opted-in `compact` value still draws the idle, planning, running and
    /// discovery letter-tile rails.
    #[test]
    fn compact_mode_idle_rail_with_every_tile_muted() {
        let mut app = app();
        app.tui.rail_mode = RailMode::Compact;
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    #[test]
    fn compact_mode_planning_rail() {
        let mut app = app();
        app.tui.rail_mode = RailMode::Compact;
        app.apply(EngineEvent::AgentChanged {
            agent: "planner".into(),
            started_ms: 0,
        });
        app.apply(EngineEvent::PlanningChanged { planning: true });
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    #[test]
    fn compact_mode_running_rail_mid_plan_then_build() {
        let mut app = running_app();
        app.tui.rail_mode = RailMode::Compact;
        app.apply(EngineEvent::PipelineChanged {
            state: stage_state(TileStatus::Active, TileStatus::Pending),
        });
        insta::assert_snapshot!(draw(&app, 80, 24));
    }

    #[test]
    fn compact_mode_discovery_rail_with_the_discover_tile_accent() {
        let mut app = app();
        app.tui.rail_mode = RailMode::Compact;
        app.apply(EngineEvent::DiscoveryChanged { discovering: true });
        app.apply(EngineEvent::PipelineChanged {
            state: PipelineState {
                discover: TileStatus::Active,
                ..stage_state(TileStatus::Muted, TileStatus::Muted)
            },
        });
        insta::assert_snapshot!(draw(&app, 80, 24));
    }
}
