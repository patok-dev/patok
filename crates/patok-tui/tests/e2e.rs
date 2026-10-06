//! The real gRPC protocol over a Unix socket: client from this crate, engine with a mock provider.

use std::sync::Arc;
use std::time::Duration;

use patok_core::event::{AgentEvent, EngineEvent, Phase, Snapshot, TaskOutcome};
use patok_engine::{Engine, EngineConfig};
use patok_proto::{
    AttachRequest, CancelSoftStop, CommandRequest, InjectTask, RoleModel, SettingsChange,
    ShutdownRequest, StartBuild, command_request, engine_update, settings_change,
    settings_change_result, shutdown_request, shutdown_update,
};
use patok_providers::mock::{MockProvider, Step};

struct Setup {
    _dirs: Vec<tempfile::TempDir>,
    config: EngineConfig,
}

fn setup() -> Setup {
    let project = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("TASKS.md"), "- [ ] T1.1: add hello\n").unwrap();
    let config = EngineConfig {
        project_dir: project.path().to_path_buf(),
        runtime_dir: runtime.path().join("e"),
        data_dir: data.path().to_path_buf(),
        agent_timeout: Duration::from_secs(30),
        idle_shutdown: Duration::from_secs(3600),
        discovery_cooldown: EngineConfig::DEFAULT_DISCOVERY_COOLDOWN,
        discovery_cooldown_cap: EngineConfig::DEFAULT_DISCOVERY_COOLDOWN_CAP,
        plan_enabled: false,
        run_mode: patok_core::config::RunMode::Sprint,
        skip_research_for_simple: true,
        review_in_loop: false,
        review_history: data.path().join("review-history.json"),
        config_files: patok_core::config::ConfigFiles::default(),
    };
    Setup {
        _dirs: vec![project, runtime, data],
        config,
    }
}

async fn connect_retrying(
    socket: &std::path::Path,
) -> patok_proto::engine_client::EngineClient<tonic::transport::Channel> {
    for _ in 0..100 {
        if let Ok(client) = patok_tui::client::connect(socket).await {
            return client;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("engine never became reachable");
}

fn snapshot_of(update: patok_proto::EngineUpdate) -> (String, Snapshot) {
    let Some(engine_update::Payload::Info(info)) = update.payload else {
        panic!("first update must be info")
    };
    (
        info.engine_version,
        Snapshot::from_state(&info.snapshot.unwrap().state).unwrap(),
    )
}

#[tokio::test]
async fn attach_build_reattach_and_soft_stop_keeps_the_engine_running() {
    let setup = setup();
    let socket = setup.config.socket_path();
    let provider = MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "hello from the agent".into(),
        }),
        Step::Event(AgentEvent::Result { text: "ok".into() }),
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest {
            shell_version: "test".into(),
        })
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
    assert_eq!(snapshot.phase, Phase::Startup);
    assert_eq!(snapshot.tasks.len(), 1);

    // Only one controlling shell at a time.
    let mut second = connect_retrying(&socket).await;
    let rejected = second.attach(AttachRequest::default()).await.unwrap_err();
    assert_eq!(rejected.code(), tonic::Code::FailedPrecondition);
    assert!(rejected.message().contains("already attached"));

    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);

    let mut saw_agent_text = false;
    let finished = loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("build events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        match EngineEvent::from_payload(&event.payload).unwrap() {
            EngineEvent::Agent {
                event: AgentEvent::Text { text },
            } if text == "hello from the agent" => saw_agent_text = true,
            EngineEvent::TaskFinished { id, outcome, .. } => break (id, outcome),
            _ => {}
        }
    };
    assert!(saw_agent_text);
    assert_eq!(finished, ("T1.1".to_string(), TaskOutcome::Done));

    // Close the shell and reattach: the engine kept the state.
    drop(updates);
    let reattached = loop {
        match client.attach(AttachRequest::default()).await {
            Ok(stream) => break stream.into_inner(),
            // The engine frees the slot asynchronously once it notices the disconnect.
            Err(status) if status.code() == tonic::Code::FailedPrecondition => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(status) => panic!("{status}"),
        }
    };
    let mut reattached = reattached;
    let (_, snapshot) = snapshot_of(reattached.message().await.unwrap().unwrap());
    assert!(snapshot.tasks[0].done);
    assert!(snapshot.recent.iter().any(|e| matches!(
        e,
        EngineEvent::Agent {
            event: AgentEvent::Text { .. }
        }
    )));

    // Soft stop: the progress ends with COMPLETE, but this is a stop, not a
    // quit -- the engine stays reachable and keeps answering commands.
    let mut progress = client
        .shutdown(ShutdownRequest::default())
        .await
        .unwrap()
        .into_inner();
    let mut last = None;
    while let Some(update) = progress.message().await.unwrap() {
        last = Some(update);
    }
    let last = last.unwrap();
    assert_eq!(last.phase, shutdown_update::Phase::Complete as i32);
    assert!(last.message.contains("keeps running"), "{}", last.message);
    // No shutdown notice reaches the attached shell: the stream stays quiet.
    if let Ok(update) = tokio::time::timeout(Duration::from_millis(300), reattached.message()).await
    {
        panic!("a soft stop must not notify the shell: {update:?}");
    }
    assert!(
        socket.exists(),
        "the engine keeps serving after a soft stop"
    );
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(!response.accepted);
    assert_eq!(response.error, "no pending tasks in TASKS.md");

    // The NOW scope while idle is the true quit: the notice arrives and the
    // engine exits.
    let mut progress = client
        .shutdown(ShutdownRequest {
            scope: shutdown_request::Scope::Now as i32,
        })
        .await
        .unwrap()
        .into_inner();
    while progress.message().await.unwrap().is_some() {}
    let notice = loop {
        match reattached.message().await.unwrap() {
            Some(patok_proto::EngineUpdate {
                payload: Some(engine_update::Payload::ShutdownNotice(n)),
            }) => break n,
            Some(_) => {}
            None => panic!("stream ended without a shutdown notice"),
        }
    };
    assert!(!notice.reason.is_empty());
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!socket.exists(), "socket file is removed on exit");
}

#[tokio::test]
async fn soft_stop_during_a_build_keeps_the_shell_running_and_a_new_build_starts() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let setup = setup();
    std::fs::write(
        setup.config.project_dir.join("TASKS.md"),
        "- [ ] T1.1: first task\n- [ ] T1.2: second task\n",
    )
    .unwrap();
    let socket = setup.config.socket_path();
    // Every session sleeps first, so the soft stop lands mid-task.
    let provider = MockProvider::new(vec![
        Step::Sleep(Duration::from_millis(300)),
        Step::Event(AgentEvent::Result {
            text: "done".into(),
        }),
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    let mut app = patok_tui::App::new(snapshot, version);

    // Start the build and wait until the first task is running.
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("build events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        app.apply(event.clone());
        if let EngineEvent::TaskStarted { ref id, .. } = event
            && id == "T1.1"
        {
            break;
        }
    }

    // The first q soft-stops the build loop.
    let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    assert_eq!(app.on_key(key), patok_tui::Action::Quit);
    assert!(app.stopping);
    let mut progress = client
        .shutdown(ShutdownRequest {
            scope: shutdown_request::Scope::Soft as i32,
        })
        .await
        .unwrap()
        .into_inner();
    while let Some(update) = progress.message().await.unwrap() {
        // A completed SOFT stop is a stop, not a quit: the shell keeps running.
        assert!(
            patok_tui::shutdown_progress(&mut app, shutdown_request::Scope::Soft, &update)
                .is_none()
        );
    }
    assert!(!app.stopping, "keys work again after the soft stop");

    // The engine finished the running task and started nothing further.
    let mut finished = None;
    let mut second_started = false;
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("build events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        app.apply(event.clone());
        match event {
            EngineEvent::TaskStarted { id, .. } if id == "T1.2" => second_started = true,
            EngineEvent::TaskFinished { id, outcome, .. } => finished = Some((id, outcome)),
            EngineEvent::PhaseChanged {
                phase: Phase::Startup,
            } if finished.is_some() => break,
            _ => {}
        }
    }
    assert_eq!(finished.unwrap(), ("T1.1".to_string(), TaskOutcome::Done));
    assert!(!second_started, "the next pending task never started");
    assert!(app.is_idle());

    // The shell is still running: Enter starts a new build (a pending task
    // remains) and the engine accepts.
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.on_key(enter), patok_tui::Action::StartBuild);
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("build events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        if matches!(
            EngineEvent::from_payload(&event.payload).unwrap(),
            EngineEvent::TaskFinished {
                id,
                outcome: TaskOutcome::Done,
                ..
            } if id == "T1.2"
        ) {
            break;
        }
    }
    // The session may still be winding down (a scheduled discovery round runs
    // at the empty end of a session); the NOW scope only quits an idle engine.
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("session end arrives")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        if matches!(
            EngineEvent::from_payload(&event.payload).unwrap(),
            EngineEvent::PhaseChanged {
                phase: Phase::Startup,
            }
        ) {
            break;
        }
    }

    // Leave via the true quit so the engine exits.
    let mut progress = client
        .shutdown(ShutdownRequest {
            scope: shutdown_request::Scope::Now as i32,
        })
        .await
        .unwrap()
        .into_inner();
    while progress.message().await.unwrap().is_some() {}
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn choosing_interrupt_from_the_stop_modal_keeps_the_shell_running_and_a_new_build_starts() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let setup = setup();
    std::fs::write(
        setup.config.project_dir.join("TASKS.md"),
        "- [ ] T1.1: first task\n- [ ] T1.2: second task\n",
    )
    .unwrap();
    let socket = setup.config.socket_path();
    // Every session sleeps first, so the interrupt lands mid-task.
    let provider = MockProvider::new(vec![
        Step::Sleep(Duration::from_millis(300)),
        Step::Event(AgentEvent::Result {
            text: "done".into(),
        }),
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    let mut app = patok_tui::App::new(snapshot, version);

    // Start the build and wait until the first task is running.
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("build events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        app.apply(event.clone());
        if let EngineEvent::TaskStarted { ref id, .. } = event
            && id == "T1.1"
        {
            break;
        }
    }

    // Esc opens the stop dialog (T46.1); Down + Enter chooses the interrupt.
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        patok_tui::Action::None
    );
    assert!(app.stop_open);
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        patok_tui::Action::None
    );
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        patok_tui::Action::Interrupt
    );
    assert!(!app.stop_open);
    assert!(app.stopping);
    let status = app.status.as_deref().unwrap();
    assert!(
        status.contains("cancelled") && status.contains("keeps running"),
        "status: {status}"
    );

    // The driver sends the NOW-scope shutdown and treats its completion as an
    // interrupt-stop, not a quit: the shell stays open.
    let mut progress = client
        .shutdown(ShutdownRequest {
            scope: shutdown_request::Scope::Now as i32,
        })
        .await
        .unwrap()
        .into_inner();
    let mut last = None;
    while let Some(update) = progress.message().await.unwrap() {
        assert!(
            patok_tui::shutdown_progress(&mut app, shutdown_request::Scope::Now, &update).is_none()
        );
        last = Some(update);
    }
    let last = last.unwrap();
    assert_eq!(last.phase, shutdown_update::Phase::Complete as i32);
    assert!(
        last.message.contains("cancelled") && last.message.contains("keeps running"),
        "message: {}",
        last.message
    );
    assert!(!app.stopping, "keys work again after the interrupt");

    // The engine cancelled the running task and started nothing further; the
    // merged view shows the cancelled task pending in the idle task list.
    let mut finished = None;
    let mut second_started = false;
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("build events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        app.apply(event.clone());
        match event {
            EngineEvent::TaskStarted { id, .. } if id == "T1.2" => second_started = true,
            EngineEvent::TaskFinished { id, outcome, .. } => finished = Some((id, outcome)),
            EngineEvent::PhaseChanged {
                phase: Phase::Startup,
            } if finished.is_some() => break,
            _ => {}
        }
    }
    assert_eq!(
        finished.unwrap(),
        ("T1.1".to_string(), TaskOutcome::Cancelled)
    );
    assert!(!second_started, "the next pending task never started");
    assert!(app.is_idle());
    assert!(app.tasks.iter().any(|t| t.id == "T1.1" && !t.done));

    // No shutdown notice reaches the attached shell: the engine keeps serving
    // (the typed idle run-state update is expected, a notice is not).
    if let Ok(update) = tokio::time::timeout(Duration::from_millis(300), updates.message()).await {
        let update = update.unwrap().expect("stream open");
        assert!(
            !matches!(
                update.payload,
                Some(engine_update::Payload::ShutdownNotice(_))
            ),
            "an interrupt must not notify the shell: {update:?}"
        );
    }
    assert!(
        socket.exists(),
        "the engine keeps serving after an interrupt"
    );

    // The shell is still running: Enter starts a new build (the interrupted
    // task is still pending) and the engine accepts.
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        patok_tui::Action::StartBuild
    );
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("build events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        if matches!(
            EngineEvent::from_payload(&event.payload).unwrap(),
            EngineEvent::TaskFinished {
                id,
                outcome: TaskOutcome::Done,
                ..
            } if id == "T1.2"
        ) {
            break;
        }
    }
    // The session may still be winding down (a scheduled discovery round runs
    // at the empty end of a session); the NOW scope only quits an idle engine.
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("session end arrives")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        if matches!(
            EngineEvent::from_payload(&event.payload).unwrap(),
            EngineEvent::PhaseChanged {
                phase: Phase::Startup,
            }
        ) {
            break;
        }
    }

    // Leave via the true quit so the engine exits.
    let mut progress = client
        .shutdown(ShutdownRequest {
            scope: shutdown_request::Scope::Now as i32,
        })
        .await
        .unwrap()
        .into_inner();
    while progress.message().await.unwrap().is_some() {}
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_cancelled_soft_stop_continues_the_loop_over_grpc() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let setup = setup();
    std::fs::write(
        setup.config.project_dir.join("TASKS.md"),
        "- [ ] T1.1: first task\n- [ ] T1.2: second task\n",
    )
    .unwrap();
    let socket = setup.config.socket_path();
    // Every session sleeps first, so the cancel lands mid-task.
    let provider = MockProvider::new(vec![
        Step::Sleep(Duration::from_millis(300)),
        Step::Event(AgentEvent::Result {
            text: "done".into(),
        }),
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    let mut app = patok_tui::App::new(snapshot, version);

    // Start the build and wait until the first task is running.
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("build events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        app.apply(event.clone());
        if let EngineEvent::TaskStarted { ref id, .. } = event
            && id == "T1.1"
        {
            break;
        }
    }

    // The first q soft-stops the build loop.
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        patok_tui::Action::Quit
    );
    assert!(app.stopping);
    let mut progress = client
        .shutdown(ShutdownRequest {
            scope: shutdown_request::Scope::Soft as i32,
        })
        .await
        .unwrap()
        .into_inner();

    // Esc cancels the pending soft stop: the command is accepted over gRPC.
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        patok_tui::Action::CancelSoftStop
    );
    assert!(!app.stopping);
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::CancelSoftStop(CancelSoftStop {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);

    // The pending SOFT shutdown stream ends with the cancelled update.
    let mut last = None;
    while let Some(update) = progress.message().await.unwrap() {
        assert!(
            patok_tui::shutdown_progress(&mut app, shutdown_request::Scope::Soft, &update)
                .is_none()
        );
        last = Some(update);
    }
    let last = last.expect("the shutdown stream ended with an update");
    assert_eq!(last.phase, shutdown_update::Phase::Complete as i32);
    assert!(
        last.message.contains("cancelled"),
        "message: {}",
        last.message
    );
    assert!(!app.stopping);

    // The loop continued with the next pending task after the running one.
    let mut cancelled_notice = false;
    let mut finished = vec![];
    let mut second_started = false;
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("build events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        app.apply(event.clone());
        match event {
            EngineEvent::Notice { text, .. } if text.contains("Soft stop cancelled") => {
                cancelled_notice = true
            }
            EngineEvent::TaskStarted { id, .. } if id == "T1.2" => second_started = true,
            EngineEvent::TaskFinished {
                id,
                outcome: TaskOutcome::Done,
                ..
            } => finished.push(id),
            EngineEvent::PhaseChanged {
                phase: Phase::Startup,
            } if second_started => break,
            _ => {}
        }
    }
    assert!(cancelled_notice, "the cancel reached the attached shell");
    assert_eq!(finished, ["T1.1".to_string(), "T1.2".to_string()]);
    assert!(!app.stopping);
    assert!(app.is_idle());

    // The keys behave normally again: with the queue complete, Enter asks
    // for a discovery round instead of a build (T46.1).
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        patok_tui::Action::RunDiscovery
    );

    // Leave via the true quit so the engine exits.
    let mut progress = client
        .shutdown(ShutdownRequest {
            scope: shutdown_request::Scope::Now as i32,
        })
        .await
        .unwrap()
        .into_inner();
    while progress.message().await.unwrap().is_some() {}
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_second_engine_for_the_same_project_refuses_to_start() {
    let setup = setup();
    let socket = setup.config.socket_path();
    let engine = Engine::new(setup.config.clone(), Arc::new(MockProvider::new(vec![])));
    let first = tokio::spawn(patok_engine::serve(engine.clone()));
    let _client = connect_retrying(&socket).await;

    let other = Engine::new(setup.config.clone(), Arc::new(MockProvider::new(vec![])));
    let error = patok_engine::serve(other).await.unwrap_err();
    assert!(error.to_string().contains("already running"), "{error}");

    engine.shutdown_token().cancel();
    first.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_stale_socket_file_is_replaced() {
    let setup = setup();
    let socket = setup.config.socket_path();
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    std::fs::write(&socket, "stale").unwrap();
    let engine = Engine::new(setup.config.clone(), Arc::new(MockProvider::new(vec![])));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));
    let _client = connect_retrying(&socket).await;
    engine.shutdown_token().cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn restart_stops_the_engine_spawns_a_fresh_one_and_reattaches() {
    use futures::FutureExt;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let setup = setup();
    let socket = setup.config.socket_path();
    let first = Engine::new(setup.config.clone(), Arc::new(MockProvider::new(vec![])));
    let old_server = tokio::spawn(patok_engine::serve(first));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    let mut app = patok_tui::App::new(snapshot, version);
    app.check_version("0.0.0-older-shell");
    assert!(app.version_mismatch);
    app.completed = 3;

    // The spawner waits for the old engine to exit, then starts a fresh one.
    let old_server = Arc::new(std::sync::Mutex::new(Some(old_server)));
    let new_server = Arc::new(std::sync::Mutex::new(None));
    let spawn: patok_tui::Spawner = {
        let (old_server, new_server) = (old_server.clone(), new_server.clone());
        let config = setup.config.clone();
        let socket = socket.clone();
        Arc::new(move || {
            let (old_server, new_server) = (old_server.clone(), new_server.clone());
            let config = config.clone();
            let socket = socket.clone();
            async move {
                let old = old_server.lock().unwrap().take().unwrap();
                old.await.unwrap().unwrap();
                let engine = Engine::new(config, Arc::new(MockProvider::new(vec![])));
                *new_server.lock().unwrap() = Some(tokio::spawn(patok_engine::serve(engine)));
                Ok(connect_retrying(&socket).await)
            }
            .boxed()
        })
    };

    let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
    tokio::time::timeout(
        Duration::from_secs(20),
        patok_tui::restart(&mut terminal, &mut app, &mut client, &mut updates, &spawn),
    )
    .await
    .expect("restart finishes")
    .unwrap();

    // The shell stayed open: session counters survive, the mismatch is gone, status says so.
    assert_eq!(app.completed, 3);
    assert!(!app.version_mismatch);
    assert_eq!(app.status.as_deref(), Some("Engine restarted."));

    // The new stream is live: a fresh attach on the new engine is rejected as already taken.
    let mut other = connect_retrying(&socket).await;
    let rejected = other.attach(AttachRequest::default()).await.unwrap_err();
    assert_eq!(rejected.code(), tonic::Code::FailedPrecondition);

    // Stop the new engine.
    let mut progress = client
        .shutdown(ShutdownRequest {
            scope: shutdown_request::Scope::Now as i32,
        })
        .await
        .unwrap()
        .into_inner();
    while progress.message().await.unwrap().is_some() {}
    let new = new_server.lock().unwrap().take().unwrap();
    tokio::time::timeout(Duration::from_secs(10), new)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn adding_tasks_through_the_dialog_shows_planner_output_and_the_new_tasks() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_proto::{AddTasks, AddTasksKind};
    use patok_tui::{Action, App, FrameFocus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let setup = setup();
    let socket = setup.config.socket_path();
    let provider = MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "Planning the new work.".into(),
        }),
        Step::WriteFile {
            path: "TASKS.md".into(),
            contents: "- [ ] T1.1: add hello\n\n## Phase 2\n- [ ] T2.1: write the greeting tests\n"
                .into(),
        },
        Step::Event(AgentEvent::Result {
            text: "done".into(),
        }),
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    let mut app = App::new(snapshot, version);

    // Open the dialog, type the request, submit.
    let press = |app: &mut App, code, modifiers| app.on_key(KeyEvent::new(code, modifiers));
    press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(app.dialog_open);
    let mut action = Action::None;
    for c in "write greeting tests".chars() {
        press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
    }
    action = match press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL) {
        Action::None => action,
        other => other,
    };
    let Action::SubmitTasks(request) = action else {
        panic!("submit key did not produce a request");
    };
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::AddTasks(AddTasks {
                request,
                kind: AddTasksKind::AddTasksPlanner as i32,
            })),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    app.on_tasks_submitted();
    assert!(!app.dialog_open, "the accepted submit closes the modal");
    assert_eq!(app.focus, FrameFocus::Output);

    let draw = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| patok_tui::render(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..24)
            .map(|y| {
                (0..100)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // Feed engine events into the app until the planner is done.
    let mut seen_planning = false;
    let mut drew_planner_frame = false;
    let mut drew_streaming_output = false;
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("planner events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        app.apply(EngineEvent::from_payload(&event.payload).unwrap());
        seen_planning |= app.planning;
        if seen_planning && app.planning {
            // The planner streams into the main agent output frame on the Dashboard tab.
            let screen = draw(&app);
            assert!(screen.contains(" PLANNING"), "{screen}");
            drew_planner_frame |= app.agent == "planner" && screen.contains(" Planner ");
            drew_streaming_output |= screen.contains("Planning the new work.");
        }
        if seen_planning && !app.planning {
            break;
        }
    }
    assert!(
        drew_planner_frame,
        "the main agent output frame carried the planner title"
    );
    assert!(
        drew_streaming_output,
        "the planner's output streamed into the main agent output frame"
    );

    // The outcome stays in the agent output frame and the status area returns to
    // its normal content: the bottom line is the key-hint strip again, so
    // nothing covers the buttons (T29.1).
    let screen = draw(&app);
    assert!(app.status.is_none(), "{screen}");
    assert!(app.is_new_task("T2.1"));
    assert_eq!(app.tasks.len(), 2);
    assert!(screen.contains("Planning the new work."), "{screen}");
    assert!(screen.contains("1 task added."), "{screen}");
    assert!(screen.contains("T2.1"), "{screen}");
    assert!(screen.contains("write the greeting tests"), "{screen}");
    let bottom = screen.lines().last().unwrap();
    assert!(bottom.contains(" settings "), "{screen}");
    // The add-tasks, scroll and Tab chips left the status line (T73.1); the
    // shorter strip fits this width whole, through the quit chip.
    assert!(!bottom.contains(" add tasks "), "{screen}");
    assert!(bottom.trim_end().ends_with(" quit"), "{screen}");
    assert!(!bottom.contains("1 task added."), "{screen}");

    engine.shutdown_token().cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_plan_stage_shows_the_planner_then_the_builder_for_one_task() {
    use patok_tui::App;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut setup = setup();
    setup.config.plan_enabled = true;
    let socket = setup.config.socket_path();
    // The plan session streams a plan that passes the gate, then the builder follows it.
    let provider = MockProvider::per_session(vec![
        vec![
            Step::Event(AgentEvent::Text {
                text: "## File Operations\nCreate greeting.txt with hello.".into(),
            }),
            Step::Event(AgentEvent::Text {
                text: "\n\n## Verification\nThe file contains hello.".into(),
            }),
        ],
        vec![
            Step::Event(AgentEvent::Text {
                text: "Creating greeting.txt.".into(),
            }),
            Step::WriteFile {
                path: "greeting.txt".into(),
                contents: "hello\n".into(),
            },
        ],
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    let mut app = App::new(snapshot, version);

    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);

    let draw = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
        terminal
            .draw(|frame| patok_tui::render(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..14)
            .map(|y| {
                (0..80)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let mut saw_plan_output = false;
    let mut saw_builder_output = false;
    let mut planner_screen = None;
    let mut builder_screen = None;
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("engine events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        saw_plan_output |= matches!(&event, EngineEvent::Agent { event: AgentEvent::Text { text } }
                if text.contains("File Operations"));
        saw_builder_output |= matches!(&event, EngineEvent::Agent { event: AgentEvent::Text { text } }
                if text.contains("Creating greeting.txt"));
        app.apply(event);
        if planner_screen.is_none() && saw_plan_output && app.agent == "planner" {
            planner_screen = Some(draw(&app));
        }
        if builder_screen.is_none() && saw_builder_output && app.agent == "builder" {
            builder_screen = Some(draw(&app));
        }
        if app.phase == Phase::Startup && app.completed == 1 {
            break;
        }
    }

    // The planner phase: the status line's chips and the frame title carry the planner, the plan is visible.
    let planner = planner_screen.expect("the planner phase was on screen");
    assert!(planner.contains(" RUNNING  sprint"), "{planner}");
    assert!(planner.contains(" Planner "), "{planner}");
    assert!(planner.contains("File Operations"), "{planner}");

    // The builder phase follows for the same task, with its own output.
    let builder = builder_screen.expect("the builder phase was on screen");
    assert!(builder.contains(" RUNNING  sprint"), "{builder}");
    assert!(builder.contains(" Builder "), "{builder}");
    assert!(builder.contains("Creating greeting.txt."), "{builder}");

    // The task completed and the builder did the planned work.
    assert!(app.tasks.iter().any(|t| t.id == "T1.1" && t.done));
    assert_eq!(
        std::fs::read_to_string(setup.config.project_dir.join("greeting.txt")).unwrap(),
        "hello\n"
    );

    engine.shutdown_token().cancel();
    server.await.unwrap().unwrap();
}

/// One agent session's end reaches an attached shell over the wire as a
/// dedicated `agent_finished` event carrying the agent type and the duration,
/// and the shell renders the finished line from it (T42.1).
#[tokio::test]
async fn a_session_end_reaches_an_attached_client_with_the_agent_and_duration() {
    use patok_core::event::SessionOutcome;
    use patok_tui::{App, FrameFocus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let setup = setup();
    let socket = setup.config.socket_path();
    // The scripted sleep makes the engine-recorded duration measurably nonzero.
    let provider = MockProvider::new(vec![
        Step::Sleep(Duration::from_millis(150)),
        Step::WriteFile {
            path: "greeting.txt".into(),
            contents: "hello\n".into(),
        },
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    let mut app = App::new(snapshot, version);

    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);

    let mut builder_end = None;
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("engine events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let decoded = EngineEvent::from_payload(&event.payload).unwrap();
        if event.kind == "agent_finished"
            && matches!(
                &decoded,
                EngineEvent::AgentFinished { agent, .. } if agent == "builder"
            )
        {
            builder_end = Some(decoded.clone());
        }
        app.apply(decoded);
        if app.phase == Phase::Startup && app.completed == 1 {
            break;
        }
    }

    // The wire event carries the agent type, the outcome and a nonzero duration.
    assert!(matches!(
        builder_end,
        Some(EngineEvent::AgentFinished {
            agent,
            outcome: SessionOutcome::Finished,
            duration_ms,
        }) if agent == "builder" && duration_ms >= 100
    ));
    // And the shell shows the finished line, with the title timer hidden.
    // A 100-column terminal: the rail's fixed column narrows the pane, and
    // this width keeps the pane's lines unwrapped. The output frame is given
    // the focus so it takes the free space: the engine appends the review-skip
    // and discovery lines after the finished line, and the fixed output pane
    // would scroll the finished line out of view.
    app.focus = FrameFocus::Output;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| patok_tui::render(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let screen = (0..24)
        .map(|y| {
            (0..100)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("Builder finished in"), "{screen}");
    assert!(!screen.contains("00:00"), "{screen}");

    engine.shutdown_token().cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_discovery_round_shows_the_chip_and_adds_its_task_to_the_list() {
    use patok_proto::RunState;
    use patok_tui::App;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let setup = setup();
    let socket = setup.config.socket_path();
    // The scheduled discovery round is continuous-mode behaviour (T103.1), so
    // this test's engine runs continuous.
    let mut config = setup.config.clone();
    config.run_mode = patok_core::config::RunMode::Continuous;
    // The completed task's line carries the five-position progress indicator
    // the engine writes at finalization (T70.1).
    let built = "- [x] T1.1: [--.B-] add hello\n";
    let provider = MockProvider::per_session(vec![
        // The build session finishes the only task.
        vec![Step::WriteFile {
            path: "greeting.txt".into(),
            contents: "hello\n".into(),
        }],
        // The scheduled discovery round appends one D task to the emptied queue.
        vec![
            Step::Event(AgentEvent::Text {
                text: "Scanning the project for follow-up work.".into(),
            }),
            Step::WriteFile {
                path: "TASKS.md".into(),
                contents: format!("{built}- [ ] D1.1: fix the greeting\n"),
            },
        ],
        // The session continues with the discovered task.
        vec![Step::WriteFile {
            path: "polish.txt".into(),
            contents: "polished\n".into(),
        }],
    ]);
    let engine = Engine::new(config, Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let first = updates.message().await.unwrap().unwrap();
    let Some(engine_update::Payload::Info(info)) = first.payload else {
        panic!("first update must be info");
    };
    let mut app = App::new(
        Snapshot::from_state(&info.snapshot.clone().unwrap().state).unwrap(),
        info.engine_version.clone(),
    );
    assert!(!app.discovering);
    // The typed run state is idle at attach, before anything runs.
    assert_eq!(info.snapshot.unwrap().run_state, RunState::Idle as i32);

    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);

    let draw = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
        terminal
            .draw(|frame| patok_tui::render(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..14)
            .map(|y| {
                (0..80)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let mut saw_discovering = false;
    let mut chip_snapshot_taken = false;
    let mut highlighted = false;
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("engine events arrive")
            .unwrap()
            .expect("stream open");
        match update.payload {
            Some(engine_update::Payload::Event(event)) => {
                app.apply(EngineEvent::from_payload(&event.payload).unwrap());
            }
            // The typed run state announces the round on the update stream.
            Some(engine_update::Payload::RunState(state)) => {
                saw_discovering |= state.state == RunState::Discovering as i32;
            }
            _ => {}
        }
        if app.discovering && !chip_snapshot_taken {
            chip_snapshot_taken = true;
            let screen = draw(&app);
            assert!(screen.contains(" DISCOVERING"), "{screen}");
            insta::assert_snapshot!(screen);
        }
        if app.is_new_task("D1.1") {
            highlighted = true;
        }
        if app.phase == Phase::Startup && app.completed == 2 {
            break;
        }
    }
    assert!(saw_discovering, "the round reached the typed run state");
    assert!(
        highlighted,
        "the discovered task reused the new-task highlight"
    );

    // The discovered task was built in the same session; the list shows it done.
    assert!(app.tasks.iter().any(|t| t.id == "D1.1" && t.done));
    assert!(draw(&app).contains("STOPPED"));
    let tasks = std::fs::read_to_string(setup.config.project_dir.join("TASKS.md")).unwrap();
    assert_eq!(
        tasks,
        "- [x] T1.1: [--.B-] add hello\n- [x] D1.1: [--.B-] fix the greeting\n"
    );

    engine.shutdown_token().cancel();
    server.await.unwrap().unwrap();
}

/// The Enter key while the engine is idle and no uncompleted task remains
/// (T17.1, reworked by T46.1): it sends the RunDiscovery command and the
/// round streams, shows the DISCOVERING chip and appends a highlighted
/// D task.
#[tokio::test]
async fn enter_while_idle_runs_a_discovery_round_and_adds_tasks() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_proto::RunState;
    use patok_tui::{Action, App};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let setup = setup();
    let socket = setup.config.socket_path();
    // Enter only runs a discovery round when the queue is complete (T46.1),
    // so the seeded pending task is ticked before the engine starts.
    let tasks = "- [x] T1.1: add hello\n";
    std::fs::write(setup.config.project_dir.join("TASKS.md"), tasks).unwrap();
    let provider = MockProvider::new(vec![
        Step::Event(AgentEvent::Text {
            text: "Scanning the project for follow-up work.".into(),
        }),
        Step::WriteFile {
            path: "TASKS.md".into(),
            contents: format!("{tasks}- [ ] D1.1: fix the greeting\n"),
        },
        Step::Event(AgentEvent::Result {
            text: "one follow-up found".into(),
        }),
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let first = updates.message().await.unwrap().unwrap();
    let Some(engine_update::Payload::Info(info)) = first.payload else {
        panic!("first update must be info");
    };
    let mut app = App::new(
        Snapshot::from_state(&info.snapshot.clone().unwrap().state).unwrap(),
        info.engine_version.clone(),
    );
    assert_eq!(app.focus, patok_tui::FrameFocus::Tasks);
    assert!(!app.discovering);

    // Enter no longer opens the dialog: it asks for a discovery round.
    let action = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(action, Action::RunDiscovery);
    assert!(!app.dialog_open);
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::RunDiscovery(
                patok_proto::RunDiscovery {},
            )),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);

    let draw = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
        terminal
            .draw(|frame| patok_tui::render(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..14)
            .map(|y| {
                (0..80)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // Watch the round run: the DISCOVERING chip while it is active, the streamed
    // output, the notice, then the appended and highlighted D task.
    let mut saw_discovering = false;
    let mut saw_chip = false;
    let mut saw_output = false;
    let mut saw_notice = false;
    let mut highlighted = false;
    let mut chip_screen = None;
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("engine events arrive")
            .unwrap()
            .expect("stream open");
        match update.payload {
            Some(engine_update::Payload::Event(event)) => {
                let event = EngineEvent::from_payload(&event.payload).unwrap();
                saw_output |= matches!(&event, EngineEvent::Agent {
                    event: AgentEvent::Text { text },
                } if text.contains("Scanning the project"));
                if matches!(&event, EngineEvent::Notice { text, .. } if text == "1 task added.") {
                    saw_notice = true;
                }
                app.apply(event);
            }
            Some(engine_update::Payload::RunState(state)) => {
                saw_discovering |= state.state == RunState::Discovering as i32;
            }
            _ => {}
        }
        if app.discovering && !saw_chip {
            saw_chip = true;
            chip_screen = Some(draw(&app));
        }
        if app.is_new_task("D1.1") {
            highlighted = true;
        }
        if saw_discovering && !app.discovering && !app.tasks.is_empty() {
            break;
        }
    }

    assert!(saw_discovering, "the round reached the typed run state");
    let chip = chip_screen.expect("the chip was on screen");
    assert!(chip.contains(" DISCOVERING"), "{chip}");
    assert!(saw_output, "the discovery output reached the pane");
    assert!(saw_notice, "the added-task notice arrived");
    assert!(
        highlighted,
        "the discovered task reused the new-task highlight"
    );
    assert!(app.tasks.iter().any(|t| t.id == "D1.1" && !t.done));
    let screen = draw(&app);
    assert!(screen.contains("D1.1"), "{screen}");
    assert!(screen.contains("fix the greeting"), "{screen}");
    let file = std::fs::read_to_string(setup.config.project_dir.join("TASKS.md")).unwrap();
    assert_eq!(file, format!("{tasks}- [ ] D1.1: fix the greeting\n"));

    engine.shutdown_token().cancel();
    server.await.unwrap().unwrap();
}

/// The merged view's default frame focus (T30.1) against a live engine: the task
/// list is focused at attach, the output frame once a build runs, and the task
/// list again once the engine returns to idle.
#[tokio::test]
async fn the_frame_focus_follows_the_engine_state() {
    use patok_tui::{App, FrameFocus};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let setup = setup();
    let socket = setup.config.socket_path();
    let provider = MockProvider::new(vec![Step::WriteFile {
        path: "greeting.txt".into(),
        contents: "hello\n".into(),
    }]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    let mut app = App::new(snapshot, version);

    let draw = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| patok_tui::render(frame, app))
            .unwrap();
    };

    // Idle at attach: the pipeline rail takes its fixed column left of the
    // frames (T53.1), the task list takes the free space, the output frame
    // shrinks to 5 content lines.
    draw(&app);
    assert_eq!(app.focus, FrameFocus::Tasks);
    assert_eq!(app.output_area.get().height, 7);
    assert_eq!(app.tasks_area.get().height, 16);

    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);

    let mut saw_running_focus = false;
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("build events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        app.apply(EngineEvent::from_payload(&event.payload).unwrap());
        if app.phase == Phase::Running {
            draw(&app);
            saw_running_focus |=
                app.focus == FrameFocus::Output && app.tasks_area.get().height == 7;
        }
        if app.phase == Phase::Startup && app.completed == 1 {
            break;
        }
    }
    assert!(
        saw_running_focus,
        "a running build focuses the output frame"
    );

    // Back to idle: the task list takes the default focus again.
    draw(&app);
    assert_eq!(app.focus, FrameFocus::Tasks);
    assert_eq!(app.output_area.get().height, 7);
    assert_eq!(app.tasks_area.get().height, 16);

    engine.shutdown_token().cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_settings_change_round_trips_over_grpc_and_updates_the_shell() {
    use patok_proto::Role;

    let setup = setup();
    let socket = setup.config.socket_path();
    let engine = Engine::new(setup.config.clone(), Arc::new(MockProvider::new(vec![])));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    let mut app = patok_tui::App::new(snapshot, version);
    assert_eq!(app.model, "");

    // A valid change: accepted, written to the project-local layer, with its apply timing.
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::SettingsChange(SettingsChange {
                change: Some(settings_change::Change::ModelOverride("opus".into())),
            })),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    let result = response.settings_change_result.unwrap();
    assert_eq!(
        result.written_layer,
        settings_change_result::Layer::ProjectLocal as i32
    );
    assert_eq!(
        result.apply_timing,
        settings_change_result::ApplyTiming::NextUnitOfWork as i32
    );
    let text = std::fs::read_to_string(setup.config.data_dir.join("patok.config.toml")).unwrap();
    assert!(text.contains("model = \"opus\""), "{text}");

    // The broadcast reaches the attached shell and syncs its display.
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("the broadcast arrives")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        let is_model_change = matches!(
            &event,
            EngineEvent::ConfigChanged { field, .. } if field == "model"
        );
        app.apply(event);
        if is_model_change {
            break;
        }
    }
    assert_eq!(app.model, "opus");
    // A non-display field arrives as a notice line with its apply-timing hint.
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::SettingsChange(SettingsChange {
                change: Some(settings_change::Change::AgentTimeoutSecs(120)),
            })),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("the broadcast arrives")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        // The first reload's flip of the test config's seeded values also arrives; wait
        // for the submitted change's own report.
        let is_timeout_change = matches!(
            &event,
            EngineEvent::ConfigChanged {
                field,
                value: patok_core::config::SettingValue::Uint(120),
                ..
            } if field == "agent_timeout_secs"
        );
        app.apply(event);
        if is_timeout_change {
            break;
        }
    }
    let lines = app
        .output
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>();
    assert!(
        lines.contains(
            &"Setting `agent_timeout_secs` changed to `120` (takes effect on the next start)"
        ),
        "{lines:?}"
    );

    // Clearing the model is accepted and reported back as cleared.
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::SettingsChange(SettingsChange {
                change: Some(settings_change::Change::ModelOverride(String::new())),
            })),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    assert!(response.settings_change_result.is_some());

    // An orchestrator role seat has no daemon registry key: rejected with a clear error.
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::SettingsChange(SettingsChange {
                change: Some(settings_change::Change::RoleModel(RoleModel {
                    role: Role::OrchestratorProposer as i32,
                    model: "opus".into(),
                })),
            })),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(!response.accepted);
    assert!(
        response.error.contains("unknown daemon setting"),
        "{}",
        response.error
    );
    assert!(response.settings_change_result.is_none());

    engine.shutdown_token().cancel();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn the_settings_overlay_drafts_saves_and_discards_changes() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_core::config::SettingValue;
    use patok_proto::SettingsChange;
    use patok_proto::command_request;
    use patok_tui::{Action, App, Entry, to_proto};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let setup = setup();
    let socket = setup.config.socket_path();
    let engine = Engine::new(setup.config.clone(), Arc::new(MockProvider::new(vec![])));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    // The attach snapshot carries the daemon readout: the overlay renders the engine's
    // effective values, not the file defaults.
    assert_eq!(
        snapshot.settings.get("run_mode"),
        Some(&SettingValue::Str("sprint".into()))
    );
    assert_eq!(
        snapshot.settings.get("agent_timeout_secs"),
        Some(&SettingValue::Uint(30))
    );
    assert_eq!(
        snapshot.settings.get("plan_enabled"),
        Some(&SettingValue::Bool(false))
    );
    let mut app = App::new(snapshot, version);

    let press = |app: &mut App, code: KeyCode| app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    let focus_field = |app: &mut App, field: &str| {
        let index = app
            .overlay
            .visible()
            .iter()
            .position(|entry| matches!(entry, Entry::Row(row) if row.key == field))
            .unwrap_or_else(|| panic!("`{field}` is not visible"));
        app.overlay.focus = index;
    };
    let draw = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal
            .draw(|frame| patok_tui::render(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..40)
            .map(|y| {
                (0..100)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // Open the overlay and toggle plan enabled: the change lands in the draft only --
    // no settings command, no file write, no applied value.
    assert_eq!(press(&mut app, KeyCode::Char('?')), Action::None);
    focus_field(&mut app, "plan_enabled");
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert_eq!(
        app.settings.get("plan_enabled"),
        Some(&SettingValue::Bool(false))
    );
    assert!(app.overlay.dirty());
    let config_file = setup.config.data_dir.join("patok.config.toml");
    // Nothing was persisted while editing: no config file appeared.
    assert!(!config_file.exists());
    let screen = draw(&app);
    assert!(screen.contains("[x] plan enabled"), "{screen}");
    assert!(!screen.contains("Saved."), "{screen}");

    // Esc opens the three-choice dialog; Enter on Save asks the driver to apply
    // every drafted change through the settings-change flow. Simulating it (the
    // driver loops pending() through the flow) persists the change.
    assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    assert!(app.overlay.confirm_open);
    assert_eq!(press(&mut app, KeyCode::Enter), Action::SaveSettings);
    for (schema, field, value) in app.overlay.pending() {
        let change = to_proto(&field, value.clone()).unwrap();
        let response = client
            .submit_command(CommandRequest {
                action: Some(command_request::Action::SettingsChange(SettingsChange {
                    change: Some(change),
                })),
            })
            .await
            .unwrap()
            .into_inner();
        assert!(response.accepted, "{}", response.error);
        app.on_settings_applied(schema, &field, value);
    }
    app.overlay.close();
    let saved = std::fs::read_to_string(&config_file).unwrap();
    assert!(saved.contains("plan_enabled = true"), "{saved}");

    // The engine's broadcast syncs the open overlay's row from the reported value.
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("the broadcast arrives")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        let is_plan = matches!(
            &event,
            EngineEvent::ConfigChanged { field, value: SettingValue::Bool(true), .. }
                if field == "plan_enabled"
        );
        app.apply(event);
        if is_plan {
            break;
        }
    }
    assert_eq!(
        app.settings.get("plan_enabled"),
        Some(&SettingValue::Bool(true))
    );

    // The run-mode chip syncs only after a save: drafting continuous moves nothing.
    assert_eq!(press(&mut app, KeyCode::Char('?')), Action::None);
    focus_field(&mut app, "run_mode");
    assert_eq!(press(&mut app, KeyCode::Right), Action::None);
    assert!(
        draw(&app).contains(" STOPPED  sprint "),
        "the chip did not move"
    );
    assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
    assert_eq!(press(&mut app, KeyCode::Enter), Action::SaveSettings);
    for (schema, field, value) in app.overlay.pending() {
        let change = to_proto(&field, value.clone()).unwrap();
        let response = client
            .submit_command(CommandRequest {
                action: Some(command_request::Action::SettingsChange(SettingsChange {
                    change: Some(change),
                })),
            })
            .await
            .unwrap()
            .into_inner();
        assert!(response.accepted, "{}", response.error);
        app.on_settings_applied(schema, &field, value);
    }
    app.overlay.close();
    assert!(
        draw(&app).contains(" STOPPED  continuous "),
        "the chip follows the save"
    );
    let saved = std::fs::read_to_string(&config_file).unwrap();
    assert!(saved.contains("run_mode = \"continuous\""), "{saved}");

    // The engine's broadcast is idempotent for the chip: it reports the same value.
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("the run-mode broadcast arrives")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        let is_run_mode = matches!(
            &event,
            EngineEvent::ConfigChanged { field, value: SettingValue::Str(mode), .. }
                if field == "run_mode" && mode == "continuous"
        );
        app.apply(event);
        if is_run_mode {
            break;
        }
    }
    assert!(
        draw(&app).contains(" STOPPED  continuous "),
        "still in sync"
    );

    // Reopen, draft a change back, then close with Discard: nothing is submitted,
    // no file is written and the saved values stand.
    assert_eq!(press(&mut app, KeyCode::Char('?')), Action::None);
    focus_field(&mut app, "plan_enabled");
    assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
    assert_eq!(
        app.overlay.drafts.get("plan_enabled"),
        Some(&SettingValue::Bool(false))
    );
    assert_eq!(std::fs::read_to_string(&config_file).unwrap(), saved);
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Down);
    assert_eq!(press(&mut app, KeyCode::Enter), Action::CloseSettings);
    assert!(!app.settings_open());
    assert!(app.overlay.drafts.is_empty());
    // The file keeps the saved change and the readout never moved.
    assert_eq!(std::fs::read_to_string(&config_file).unwrap(), saved);
    assert_eq!(
        app.settings.get("plan_enabled"),
        Some(&SettingValue::Bool(true))
    );
    assert_eq!(app.run_mode(), "continuous");

    engine.shutdown_token().cancel();
    server.await.unwrap().unwrap();
}

/// S-Tab on the main view flips the run mode through the settings-change flow
/// (T60.1): the command is accepted with its apply timing, the status line's chip and
/// the status message reflect the new mode, and the value persists to the
/// project-local config file, so it survives a restart.
#[tokio::test]
async fn shift_tab_flips_the_run_mode_over_grpc() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_core::config::{RunMode, SettingValue};
    use patok_tui::{Action, App, Schema, to_proto};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let setup = setup();
    let socket = setup.config.socket_path();
    let engine = Engine::new(setup.config.clone(), Arc::new(MockProvider::new(vec![])));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest::default())
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    let mut app = App::new(snapshot, version);
    assert_eq!(app.run_mode(), "sprint");

    let draw = |app: &App| {
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
        terminal
            .draw(|frame| patok_tui::render(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..40)
            .map(|y| {
                (0..100)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // S-Tab on the main view asks the driver to flip the run mode to the other
    // value, with the status bar naming the new mode.
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)),
        Action::SetRunMode("continuous".into())
    );
    assert_eq!(
        app.status.as_deref(),
        Some("Run mode switched to continuous.")
    );

    // The driver submits it through the settings-change flow (the same path
    // `change_setting` takes): accepted, written to the project-local layer,
    // with the run-mode field's apply timing.
    let change = to_proto("run_mode", SettingValue::Str("continuous".into())).unwrap();
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::SettingsChange(SettingsChange {
                change: Some(change),
            })),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    let result = response.settings_change_result.unwrap();
    assert_eq!(
        result.written_layer,
        settings_change_result::Layer::ProjectLocal as i32
    );
    assert_eq!(
        result.apply_timing,
        settings_change_result::ApplyTiming::NextUnitOfWork as i32
    );
    // The readout (and so the chip) follows the applied change immediately.
    app.on_settings_applied(
        Schema::Daemon,
        "run_mode",
        SettingValue::Str("continuous".into()),
    );
    let screen = draw(&app);
    assert!(screen.contains("continuous"), "{screen}");
    assert!(
        screen.contains("Run mode switched to continuous."),
        "{screen}"
    );

    // The value persists to the resolved project-local file: a restart keeps it.
    let config_file = setup.config.data_dir.join("patok.config.toml");
    let saved = std::fs::read_to_string(&config_file).unwrap();
    assert!(saved.contains("run_mode = \"continuous\""), "{saved}");

    // The engine's broadcast reports the same value, so the chip stays in sync.
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("the broadcast arrives")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let event = EngineEvent::from_payload(&event.payload).unwrap();
        let is_run_mode = matches!(
            &event,
            EngineEvent::ConfigChanged {
                field,
                value: SettingValue::Str(mode),
                ..
            } if field == "run_mode" && mode == "continuous"
        );
        app.apply(event);
        if is_run_mode {
            break;
        }
    }
    assert!(draw(&app).contains("continuous"), "still in sync");
    assert_eq!(engine.settings().run_mode, RunMode::Continuous);

    engine.shutdown_token().cancel();
    server.await.unwrap().unwrap();
}

/// T77.1: the inject command over the wire, mid-build: the confirm is
/// accepted while a builder session is running, the appended line reaches the
/// shell as a task-list change before the first task finishes, and the
/// injected task runs as the next task once the current one finishes.
#[tokio::test]
async fn inject_task_over_the_wire_mid_build_appends_and_runs_next() {
    let setup = setup();
    let socket = setup.config.socket_path();
    let provider = MockProvider::new(vec![
        Step::Sleep(Duration::from_millis(300)),
        Step::Event(AgentEvent::Result {
            text: "done".into(),
        }),
    ]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider.clone()));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest {
            shell_version: "test".into(),
        })
        .await
        .unwrap()
        .into_inner();
    let _ = snapshot_of(updates.message().await.unwrap().unwrap());

    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::StartBuild(StartBuild {})),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);

    // The builder session is mid-flight; the inject is still accepted.
    for _ in 0..500 {
        if !provider.sessions().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(provider.sessions().len(), 1);
    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::InjectTask(InjectTask {
                text: "- [ ] T2.1: injected over the wire".into(),
            })),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    let file = std::fs::read_to_string(setup.config.project_dir.join("TASKS.md")).unwrap();
    assert!(
        file.contains("- [ ] T2.1: injected over the wire\n"),
        "{file}"
    );

    // The task-list change arrives before the first task finishes, and the
    // injected task is the second one to finish.
    let mut saw_injected = false;
    let mut finished = 0;
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("build events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        match EngineEvent::from_payload(&event.payload).unwrap() {
            EngineEvent::TasksChanged { tasks } if tasks.iter().any(|t| t.id == "T2.1") => {
                saw_injected = true;
            }
            EngineEvent::TaskFinished { id, .. } => {
                finished += 1;
                if finished == 2 {
                    assert_eq!(id, "T2.1");
                    break;
                }
                assert!(
                    saw_injected,
                    "the task-list change arrived before the first finish"
                );
            }
            _ => {}
        }
    }
    assert!(saw_injected);

    engine.shutdown_token().cancel();
    server.abort();
}

/// T77.1: typing plain text into the inject modal and confirming appends a
/// well-formed unchecked task line -- the engine adds the checkbox and the
/// next `T` id -- instead of a bare text line, and no agent session starts.
#[tokio::test]
async fn injecting_plain_text_through_the_modal_appends_a_well_formed_line() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let setup = setup();
    let socket = setup.config.socket_path();
    let provider = MockProvider::new(vec![]);
    let engine = Engine::new(setup.config.clone(), Arc::new(provider.clone()));
    let server = tokio::spawn(patok_engine::serve(engine.clone()));

    let mut client = connect_retrying(&socket).await;
    let mut updates = client
        .attach(AttachRequest {
            shell_version: "test".into(),
        })
        .await
        .unwrap()
        .into_inner();
    let (version, snapshot) = snapshot_of(updates.message().await.unwrap().unwrap());
    let mut app = patok_tui::App::new(snapshot, version);

    // Drive the modal exactly as a user would: `i` opens it, the text is
    // typed, Enter confirms. The action carries the raw typed text; the
    // normalizing happens engine-side.
    app.on_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
    assert!(app.dialog_open);
    for c in "polish the readme".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        patok_tui::Action::InjectTask("polish the readme".into())
    );

    let response = client
        .submit_command(CommandRequest {
            action: Some(command_request::Action::InjectTask(InjectTask {
                text: "polish the readme".into(),
            })),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(response.accepted, "{}", response.error);
    app.on_task_injected();

    // Exactly one line was appended, in the full expected form; the
    // existing line is byte-identical.
    let file = std::fs::read_to_string(setup.config.project_dir.join("TASKS.md")).unwrap();
    assert_eq!(
        file, "- [ ] T1.1: add hello\n- [ ] T2.1: polish the readme\n",
        "{file}"
    );
    // The inject starts no agent, planner or discovery session.
    assert!(provider.sessions().is_empty());

    // The appended line reaches the shell as a task-list change, and the
    // rendered task list shows it in its full form.
    loop {
        let update = tokio::time::timeout(Duration::from_secs(20), updates.message())
            .await
            .expect("engine events arrive")
            .unwrap()
            .expect("stream open");
        let Some(engine_update::Payload::Event(event)) = update.payload else {
            continue;
        };
        let decoded = EngineEvent::from_payload(&event.payload).unwrap();
        let injected = matches!(
            &decoded,
            EngineEvent::TasksChanged { tasks } if tasks.iter().any(|t| t.id == "T2.1")
        );
        app.apply(decoded);
        if injected {
            break;
        }
    }
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| patok_tui::render(frame, &app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let screen = (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    // The rendered task list shows the injected line in the task pane's own
    // form: the new unchecked task with its fresh id.
    assert!(screen.contains("○ T2.1  polish the readme"), "{screen}");

    engine.shutdown_token().cancel();
    server.abort();
}
