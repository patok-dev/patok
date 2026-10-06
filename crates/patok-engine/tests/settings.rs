//! The settings-change flow over the engine: a submitted
//! change validated and persisted, an invalid change rejected, a hand-edited config
//! file picked up by the reload, and the apply-timing rule, all against the mock provider.

mod support;

use std::sync::Arc;
use std::time::Duration;

use patok_core::config::{ApplyTiming, DaemonEnv, ProviderKind, RunMode, SettingValue};
use patok_core::event::{AgentEvent, EngineEvent, TaskOutcome};
use patok_engine::{Engine, EngineConfig};
use patok_providers::mock::{MockProvider, Step};
use support::{Fixture, collect_until, is_phase_startup};

const TASKS: &str = "# Tasks\n\n## Phase 1\n- [ ] T1.1: add the greeting file\n";

/// The (value, timing) the engine broadcast for `field`, if it did.
fn config_change(events: &[EngineEvent], field: &str) -> Option<(SettingValue, ApplyTiming)> {
    events.iter().find_map(|event| match event {
        EngineEvent::ConfigChanged {
            field: changed,
            value,
            timing,
        } if changed == field => Some((value.clone(), *timing)),
        _ => None,
    })
}

fn project_config(fixture: &Fixture) -> String {
    std::fs::read_to_string(fixture.project_config()).unwrap()
}

/// A config whose values match the file defaults, so a submitted change is the only diff
/// the reload reports.
fn defaults_config(fixture: &Fixture) -> EngineConfig {
    let mut config = fixture.config_for(true);
    config.agent_timeout = EngineConfig::DEFAULT_AGENT_TIMEOUT;
    config.idle_shutdown = EngineConfig::DEFAULT_IDLE_SHUTDOWN;
    // The file default for the run mode is sprint (the fixture's continuous
    // default is for the scheduled-round tests, T103.1).
    config.run_mode = RunMode::Sprint;
    config
}

#[tokio::test]
async fn a_valid_settings_change_updates_the_config_the_file_and_broadcasts() {
    let fixture = Fixture::new(TASKS);
    let engine = Engine::new(
        defaults_config(&fixture),
        Arc::new(MockProvider::new(vec![])),
    );
    let mut attachment = engine.attach().unwrap();

    let timing = engine
        .apply_settings_change("agent_timeout_secs", SettingValue::Uint(120))
        .unwrap();
    assert_eq!(timing, ApplyTiming::NextUnitOfWork);

    // The central config and the resolved file carry the new value.
    assert_eq!(engine.settings().agent_timeout_secs, 120);
    let text = project_config(&fixture);
    assert!(text.contains("[daemon]"), "{text}");
    assert!(text.contains("agent_timeout_secs = 120"), "{text}");

    // The change is broadcast back with the field's effective value and apply timing.
    let events = collect_until(&mut attachment.events, |event| {
        matches!(event, EngineEvent::ConfigChanged { field, .. } if field == "agent_timeout_secs")
    })
    .await;
    assert_eq!(
        config_change(&events, "agent_timeout_secs"),
        Some((SettingValue::Uint(120), ApplyTiming::NextUnitOfWork)),
        "{events:#?}"
    );
    assert!(
        config_change(&events, "provider").is_none(),
        "only the changed field is reported"
    );
}

/// S-Tab's run-mode flip (T60.1): the same flow as any other field, through the
/// daemon registry's run-mode key.
#[tokio::test]
async fn a_run_mode_settings_change_updates_the_config_the_file_and_broadcasts() {
    let fixture = Fixture::new(TASKS);
    let engine = Engine::new(
        defaults_config(&fixture),
        Arc::new(MockProvider::new(vec![])),
    );
    let mut attachment = engine.attach().unwrap();

    let timing = engine
        .apply_settings_change("run_mode", SettingValue::Str("continuous".into()))
        .unwrap();
    assert_eq!(timing, ApplyTiming::NextUnitOfWork);

    // The central config and the resolved project-local file carry the new mode.
    assert_eq!(engine.settings().run_mode, RunMode::Continuous);
    let text = project_config(&fixture);
    assert!(text.contains("[daemon]"), "{text}");
    assert!(text.contains("run_mode = \"continuous\""), "{text}");

    // The change is broadcast back with the field's effective value and apply
    // timing, so the shell's readout (and its run-mode chip) follows.
    let events = collect_until(
        &mut attachment.events,
        |event| matches!(event, EngineEvent::ConfigChanged { field, .. } if field == "run_mode"),
    )
    .await;
    assert_eq!(
        config_change(&events, "run_mode"),
        Some((
            SettingValue::Str("continuous".into()),
            ApplyTiming::NextUnitOfWork
        )),
        "{events:#?}"
    );
    assert!(
        config_change(&events, "provider").is_none(),
        "only the changed field is reported"
    );
}

#[tokio::test]
async fn an_invalid_settings_change_is_rejected_and_changes_nothing() {
    let fixture = Fixture::new(TASKS);
    let engine = Engine::new(fixture.config(), Arc::new(MockProvider::new(vec![])));
    let mut attachment = engine.attach().unwrap();

    // Wrong type, with the same message the on-disk validation produces.
    let error = engine
        .apply_settings_change("agent_timeout_secs", SettingValue::Bool(true))
        .unwrap_err();
    assert!(error.contains("must be a whole number"), "{error}");
    let error = engine
        .apply_settings_change("plan_enabled", SettingValue::Str("yes".into()))
        .unwrap_err();
    assert!(error.contains("must be a boolean"), "{error}");
    // Unknown names are rejected, including the orchestrator seats.
    let error = engine
        .apply_settings_change("no_such_key", SettingValue::Uint(1))
        .unwrap_err();
    assert!(error.contains("unknown daemon setting"), "{error}");
    let error = engine
        .apply_settings_change("orchestrator_proposer_model", SettingValue::Str("m".into()))
        .unwrap_err();
    assert!(error.contains("unknown daemon setting"), "{error}");
    // Zero is rejected where the on-disk parser requires a positive value.
    let error = engine
        .apply_settings_change("agent_timeout_secs", SettingValue::Uint(0))
        .unwrap_err();
    assert!(error.contains("greater than zero"), "{error}");

    // Nothing was written, nothing changed, nothing was broadcast.
    assert!(!fixture.project_config().exists());
    assert_eq!(engine.settings().agent_timeout_secs, 30);
    // No event was broadcast.
    assert!(matches!(
        attachment.events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn a_hand_edited_config_file_is_picked_up_by_the_reload() {
    let fixture = Fixture::new(TASKS);
    let engine = Engine::new(
        fixture.config_for(true),
        Arc::new(MockProvider::new(vec![])),
    );
    let mut attachment = engine.attach().unwrap();
    engine.spawn_config_watch();
    // Let the watch capture the initial modification times first.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // One hand edit with a valid field and a wrong-typed one: the valid field applies,
    // the wrong-typed one falls back to its default with a warning, and both are reported
    // through the same reload as a submitted change.
    std::fs::write(
        fixture.project_config(),
        "[daemon]\nplan_enabled = false\nagent_timeout_secs = \"soon\"\n",
    )
    .unwrap();
    // Both changed fields are reported by one reload, so wait for the pair.
    let mut saw_plan_enabled = false;
    let events = collect_until(&mut attachment.events, |event| match event {
        EngineEvent::ConfigChanged { field, .. } if field == "plan_enabled" => {
            saw_plan_enabled = true;
            false
        }
        EngineEvent::ConfigChanged { field, .. } if field == "agent_timeout_secs" => {
            saw_plan_enabled
        }
        _ => false,
    })
    .await;

    assert!(!engine.settings().plan_enabled);
    assert_eq!(
        config_change(&events, "plan_enabled"),
        Some((SettingValue::Bool(false), ApplyTiming::NextUnitOfWork)),
        "{events:#?}"
    );
    // The wrong-typed field fell back to its default (600), not to the config's 30.
    assert_eq!(engine.settings().agent_timeout_secs, 600);
    assert_eq!(
        config_change(&events, "agent_timeout_secs"),
        Some((SettingValue::Uint(600), ApplyTiming::NextUnitOfWork)),
        "{events:#?}"
    );

    engine.shutdown_token().cancel();
}

#[tokio::test]
async fn a_mid_run_change_takes_effect_at_the_next_build_start() {
    let fixture = Fixture::new("- [ ] T1.1: fix the first\n");
    // The plan stage off, so the build sessions stay plain builder sessions;
    // continuous mode so the session's empty end still runs the scheduled
    // discovery round on the old unit (T103.1).
    std::fs::write(
        fixture.project_config(),
        "[daemon]\nplan_enabled = false\nrun_mode = \"continuous\"\n",
    )
    .unwrap();
    let claude = MockProvider::per_session(vec![
        // The first builder session is still running when the settings change lands.
        vec![
            Step::Sleep(Duration::from_millis(1500)),
            Step::WriteFile {
                path: "one.txt".into(),
                contents: "one\n".into(),
            },
        ],
        // The scheduled discovery round at the session's empty end.
        vec![Step::Event(AgentEvent::Result {
            text: "nothing worth doing".into(),
        })],
    ]);
    let claude_recorded = claude.clone();
    let claude = Arc::new(claude);
    let mistral = MockProvider::new(vec![Step::WriteFile {
        path: "two.txt".into(),
        contents: "two\n".into(),
    }]);
    let mistral_recorded = mistral.clone();
    let mistral = Arc::new(mistral);
    let engine = Engine::configured_with_env(
        fixture.config(),
        &DaemonEnv::default(),
        move |kind| match kind {
            ProviderKind::Claude => Ok(claude.clone()),
            ProviderKind::Codex | ProviderKind::Mistral => Ok(mistral.clone()),
        },
    );
    let mut attachment = engine.attach().unwrap();
    assert_eq!(attachment.snapshot.provider, "claude");

    engine.start_build().unwrap();
    collect_until(
        &mut attachment.events,
        |event| matches!(event, EngineEvent::TaskStarted { id, .. } if id == "T1.1"),
    )
    .await;

    // The change lands mid-run: the running session keeps the snapshot it started with,
    // and the new values apply at the next build start.
    engine
        .apply_settings_change("model", SettingValue::Str("opus".into()))
        .unwrap();
    engine
        .apply_settings_change("provider", SettingValue::Str("mistral".into()))
        .unwrap();
    let events = collect_until(
        &mut attachment.events,
        |event| matches!(event, EngineEvent::ConfigChanged { field, .. } if field == "provider"),
    )
    .await;
    assert_eq!(
        config_change(&events, "provider"),
        Some((
            SettingValue::Str("mistral".into()),
            ApplyTiming::NextUnitOfWork
        )),
        "{events:#?}"
    );

    collect_until(&mut attachment.events, is_phase_startup).await;
    let sessions = claude_recorded.sessions();
    assert_eq!(
        sessions.len(),
        2,
        "the builder and the scheduled discovery ran on the old unit"
    );
    for session in &sessions {
        assert_eq!(
            session.model, None,
            "the old unit's sessions keep its snapshot"
        );
    }
    assert!(
        mistral_recorded.sessions().is_empty(),
        "the running unit was untouched"
    );
    assert_eq!(engine.settings().model.as_deref(), Some("opus"));

    // The next build start takes the new routing.
    std::fs::write(
        fixture.path().join("TASKS.md"),
        "- [x] T1.1: fix the first\n- [ ] T1.2: fix the second\n",
    )
    .unwrap();
    engine.start_build().unwrap();
    collect_until(&mut attachment.events, |event| {
        matches!(event, EngineEvent::TaskFinished { id, outcome: TaskOutcome::Done, .. } if id == "T1.2")
    })
    .await;
    let sessions = mistral_recorded.sessions();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].model.as_deref(), Some("opus"));
    assert_eq!(
        claude_recorded.sessions().len(),
        2,
        "the old unit's provider was untouched"
    );
}

#[tokio::test]
async fn attach_reports_the_daemon_settings_readout() {
    let fixture = Fixture::new(TASKS);
    let mut config = fixture.config_for(false);
    config.agent_timeout = Duration::from_secs(45);
    let engine = Engine::new(config, Arc::new(MockProvider::new(vec![])));
    let attachment = engine.attach().unwrap();

    // Every registry field is reported with its effective value, not the file defaults:
    // the engine was seeded from its config here -- the fixture seeds continuous
    // (T103.1), while the file default is sprint.
    let readout = &attachment.snapshot.settings;
    assert!(!readout.is_empty());
    assert_eq!(
        readout.get("run_mode"),
        Some(&SettingValue::Str("continuous".into()))
    );
    assert_eq!(
        readout.get("agent_timeout_secs"),
        Some(&SettingValue::Uint(45))
    );
    assert_eq!(
        readout.get("discovery_cooldown_secs"),
        Some(&SettingValue::Uint(300))
    );
    assert_eq!(
        readout.get("plan_enabled"),
        Some(&SettingValue::Bool(false))
    );

    // A settings change is reflected in a fresh attach's readout.
    engine
        .apply_settings_change("plan_enabled", SettingValue::Bool(true))
        .unwrap();
    drop(attachment);
    let attachment = engine.attach().unwrap();
    assert_eq!(
        attachment.snapshot.settings.get("plan_enabled"),
        Some(&SettingValue::Bool(true))
    );
}

/// A legacy `patok.config.toml` in the project root (the pre-T92.1 location) is
/// picked up and migrated on the first load: the values are effective, the file
/// is copied byte-for-byte into the project's slot of the projects dir, and the
/// legacy file is left in place, untouched.
#[tokio::test]
async fn a_legacy_project_root_config_is_migrated_on_the_first_load() {
    let fixture = Fixture::new(TASKS);
    let legacy = "[daemon]\nplan_enabled = false\n";
    std::fs::write(fixture.path().join("patok.config.toml"), legacy).unwrap();
    let provider = MockProvider::new(vec![]);
    let engine = Engine::configured(fixture.config(), move |_| Ok(Arc::new(provider.clone())));

    assert!(!engine.settings().plan_enabled);
    assert_eq!(
        std::fs::read_to_string(fixture.project_config()).unwrap(),
        legacy,
        "the config was copied into the data dir"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.path().join("patok.config.toml")).unwrap(),
        legacy,
        "the legacy file is untouched"
    );
}
