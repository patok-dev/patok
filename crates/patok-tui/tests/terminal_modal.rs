//! T2.3, T2.4, T4.1: the embedded terminal -- the picker's Enter opens a PTY-backed
//! modal in the project dir, keys, paste and mouse reach the child, the modal
//! spans the full width, the size follows the screen, and Ctrl+] closes it. An exited child keeps its screen
//! with a close hint, the running footer hints that Esc reaches the child, and closing or dropping the app kills the child's whole
//! process group. The children are `sh` stubs
//! on a tempdir search path, so no result depends on the host.

mod support;

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
use patok_tui::{Action, App, Modal, PtyEvent, terminal_area};
use ratatui::layout::Rect;
use tempfile::TempDir;
use tokio::sync::mpsc::Receiver;

use support::{draw, mouse, press};

const TASKS: &str = "## Phase 1\n- [ ] T1.1: scaffold the workspace\n";

struct Fixture {
    app: App,
    rx: Receiver<PtyEvent>,
    project: TempDir,
    _stubs: TempDir,
}

fn app_in(project: &str) -> App {
    App::new(
        support::snapshot(TASKS)
            .project_dir(project)
            .provider("claude")
            .build(),
        "0.1.0".into(),
    )
}

fn stub_dir(app: &mut App, name: &str, body: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(name);
    std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&file, PermissionsExt::from_mode(0o755)).unwrap();
    let path: OsString =
        std::env::join_paths([dir.path(), "/usr/bin".as_ref(), "/bin".as_ref()]).unwrap();
    app.interactive.search_path = Some(path);
    dir
}

/// Opens the terminal on a `claude` stub running `body`.
fn open(body: &str) -> Fixture {
    let project = tempfile::tempdir().unwrap();
    let mut app = app_in(project.path().to_str().unwrap());
    let stubs = stub_dir(&mut app, "claude", body);
    draw(&app, 100, 30);
    press(&mut app, KeyCode::Char('I'));
    press(&mut app, KeyCode::Enter);
    let rx = app.take_terminal_events().expect("the terminal opened");
    Fixture {
        app,
        rx,
        project,
        _stubs: stubs,
    }
}

async fn wait_for(app: &App, rx: &mut Receiver<PtyEvent>, pred: impl Fn(&str) -> bool) {
    loop {
        let contents = app.terminal_contents().unwrap_or_default();
        if pred(&contents) {
            return;
        }
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(_)) => {}
            _ => panic!("timed out; screen: {contents:?}"),
        }
    }
}

async fn wait_exited(app: &mut App, rx: &mut Receiver<PtyEvent>) {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(PtyEvent::Exited(status))) => {
                app.on_terminal_exited(status.exit_code());
                return;
            }
            Ok(Some(_)) => {}
            other => panic!("no exit event: {other:?}"),
        }
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent::new(code, modifiers)
}

fn type_str(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

#[tokio::test]
async fn enter_opens_the_terminal_in_the_project_dir() {
    let mut f = open(r#"printf 'cwd=%s term=%s\n' "$PWD" "$TERM"; exec cat"#);
    assert_eq!(f.app.open_modals(), vec![Modal::Terminal]);
    assert!(!f.app.picker_open());
    let want = std::fs::canonicalize(f.project.path()).unwrap();
    let line = format!("cwd={} term=xterm-256color", want.display());
    wait_for(&f.app, &mut f.rx, |c| c.contains(&line)).await;
    let screen = draw(&f.app, 100, 30);
    assert!(screen.contains(&line), "{screen}");
    assert!(screen.contains(" Claude "), "{screen}");
}

#[tokio::test]
async fn keys_reach_the_child() {
    let mut f = open("exec cat");
    type_str(&mut f.app, "ping");
    press(&mut f.app, KeyCode::Enter);
    wait_for(&f.app, &mut f.rx, |c| c.matches("ping").count() >= 2).await;
}

#[tokio::test]
async fn ctrl_c_goes_to_the_child_not_detach() {
    let mut f = open("exec cat");
    assert_eq!(support::ctrl(&mut f.app, 'c'), Action::None);
    wait_exited(&mut f.app, &mut f.rx).await;
    assert!(f.app.terminal_exited());
    assert_eq!(f.app.open_modals(), vec![Modal::Terminal]);
}

#[tokio::test]
async fn ctrl_bracket_closes_and_kills_in_both_forms() {
    for ctrl_key in [
        key(KeyCode::Char(']'), KeyModifiers::CONTROL),
        key(KeyCode::Char('5'), KeyModifiers::CONTROL),
    ] {
        let mut f = open("exec cat");
        // Esc is the child's, not an exit.
        press(&mut f.app, KeyCode::Esc);
        assert_eq!(f.app.open_modals(), vec![Modal::Terminal]);
        assert_eq!(f.app.on_key(ctrl_key), Action::None);
        assert!(f.app.open_modals().is_empty());
        assert!(!f.app.terminal_open());
        let end = tokio::time::timeout(Duration::from_secs(5), async {
            while f.rx.recv().await.is_some() {}
        })
        .await;
        assert!(end.is_ok(), "the child is gone and the channel closed");
    }
}

#[tokio::test]
async fn size_follows_the_area() {
    let mut f = open("stty size; exec cat");
    let body = |w, h| {
        let inner = terminal_area(Rect::new(0, 0, w, h));
        (inner.height - 3, inner.width - 2)
    };
    let (rows, cols) = body(100, 30);
    // Before any draw the fallback size applies only without a screen; here
    // the screen was drawn first.
    let want = format!("{rows} {cols}");
    wait_for(&f.app, &mut f.rx, |c| c.contains(&want)).await;
    assert_eq!(f.app.terminal_size(), Some((rows, cols)));

    f.app.fit_terminal(Rect::new(0, 0, 80, 24));
    assert_eq!(f.app.terminal_size(), Some(body(80, 24)));
}

#[tokio::test]
async fn the_modal_spans_the_full_width() {
    // Full width, one row off the top and bottom; tiny screens get it all.
    assert_eq!(
        terminal_area(Rect::new(0, 0, 80, 24)),
        Rect::new(0, 1, 80, 22)
    );
    assert_eq!(
        terminal_area(Rect::new(0, 0, 20, 6)),
        Rect::new(0, 0, 20, 6)
    );

    let mut f = open("exec cat");
    f.app.fit_terminal(Rect::new(0, 0, 80, 24));
    // The border's inner less the footer row: 22 - 3 rows, 80 - 2 columns.
    assert_eq!(f.app.terminal_size(), Some((19, 78)));
}

#[tokio::test]
async fn mouse_is_forwarded_when_the_child_tracks_it() {
    let mut f = open(r"printf '\033[?1000h\033[?1006h'; exec cat");
    // Wait for the mode switch to be parsed, then draw to record the rects.
    tokio::time::sleep(Duration::from_millis(300)).await;
    while f.rx.try_recv().is_ok() {}
    draw(&f.app, 100, 30);
    f.app
        .on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 10, 5));
    wait_for(&f.app, &mut f.rx, |c| c.contains("[<0;")).await;
    assert!(f.app.terminal_open());

    let close = patok_tui::close_button_rect(terminal_area(Rect::new(0, 0, 100, 30)));
    f.app.on_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        close.x,
        close.y,
    ));
    assert!(!f.app.terminal_open());
    assert!(f.app.open_modals().is_empty());
}

#[tokio::test]
async fn paste_reaches_the_child() {
    let mut f = open("exec cat");
    f.app.on_paste("hello");
    wait_for(&f.app, &mut f.rx, |c| c.contains("hello")).await;
}

#[tokio::test]
async fn a_failed_spawn_sets_the_status() {
    let mut app = app_in("/home/user/demo");
    let _stubs = stub_dir(&mut app, "claude", "exec cat");
    press(&mut app, KeyCode::Char('I'));
    // The program vanishes from the search path after the picker saw it, so
    // the spawn cannot resolve it.
    let empty = tempfile::tempdir().unwrap();
    app.interactive.search_path = Some(empty.path().into());
    press(&mut app, KeyCode::Enter);
    assert!(!app.terminal_open());
    assert!(app.open_modals().is_empty());
    assert!(
        app.status
            .as_deref()
            .unwrap()
            .contains("could not start Claude"),
        "{:?}",
        app.status
    );
}

#[tokio::test]
async fn a_running_child_hints_that_esc_reaches_it() {
    let f = open("exec cat");
    let screen = draw(&f.app, 100, 30);
    assert!(screen.contains("Esc goes to the session"), "{screen}");
    assert!(screen.contains("Ctrl+]"), "{screen}");
}

#[tokio::test]
async fn the_exited_child_is_flagged_in_the_footer() {
    let mut f = open("echo done");
    wait_exited(&mut f.app, &mut f.rx).await;
    let screen = draw(&f.app, 100, 30);
    assert!(!screen.contains("Esc goes to the session"), "{screen}");
    assert!(screen.contains("done"), "{screen}");
    assert!(
        screen.contains("[process exited (code 0)] — Ctrl+] to close"),
        "{screen}"
    );
}

fn ctrl_bracket() -> KeyEvent {
    key(KeyCode::Char(']'), KeyModifiers::CONTROL)
}

/// The digits following `tag` in `contents`.
fn value(contents: &str, tag: &str) -> String {
    let rest = &contents[contents.find(tag).expect("tag on screen") + tag.len()..];
    rest.chars().take_while(char::is_ascii_digit).collect()
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

async fn gone(pid: &str) -> bool {
    for _ in 0..200 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// A stub whose background `sleep` ignores HUP, so it outlives the first
/// signal and only the SIGKILL escalation reaches it.
const GROUP_STUB: &str = r#"trap '' HUP; sleep 30 & echo "leader=$$ bg=$!"; exec cat"#;

/// Waits for the stub's pid line and returns (leader, bg).
async fn group_pids(f: &mut Fixture) -> (String, String) {
    wait_for(&f.app, &mut f.rx, |c| {
        c.lines().any(|l| {
            l.starts_with("leader=")
                && l.contains(" bg=")
                && l.ends_with(|c: char| c.is_ascii_digit())
        })
    })
    .await;
    let contents = f.app.terminal_contents().unwrap();
    let line = contents.lines().find(|l| l.starts_with("leader=")).unwrap();
    (value(line, "leader="), value(line, "bg="))
}

#[tokio::test]
async fn closing_kills_the_whole_process_group() {
    let mut f = open(GROUP_STUB);
    let (leader, bg) = group_pids(&mut f).await;
    assert!(alive(&bg));
    assert_eq!(f.app.on_key(ctrl_bracket()), Action::None);
    assert!(!f.app.terminal_open());
    assert!(f.app.open_modals().is_empty());
    assert!(gone(&leader).await, "leader {leader} survived");
    assert!(gone(&bg).await, "background {bg} survived");
}

#[tokio::test]
async fn an_exited_child_keeps_its_screen_and_closing_reaps_survivors() {
    let mut f = open(r#"trap '' HUP; sleep 30 & echo "bg=$!"; echo done; exit 3"#);
    // `Exited` can arrive before the reader thread has parsed the last output,
    // so wait for the exit first (`wait_for` would swallow it) and then poll
    // the screen.
    wait_exited(&mut f.app, &mut f.rx).await;
    let mut contents = f.app.terminal_contents().unwrap_or_default();
    for _ in 0..100 {
        if contents.contains("bg=") && contents.contains("done") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        contents = f.app.terminal_contents().unwrap_or_default();
    }
    let bg = value(&contents, "bg=");
    assert!(f.app.terminal_exited());
    assert_eq!(f.app.open_modals(), vec![Modal::Terminal]);
    assert!(alive(&bg), "the survivor outlives the leader");
    let screen = draw(&f.app, 100, 30);
    assert!(screen.contains("done"), "{screen}");
    assert!(
        screen.contains("[process exited (code 3)] — Ctrl+] to close"),
        "{screen}"
    );

    type_str(&mut f.app, "x");
    press(&mut f.app, KeyCode::Enter);
    assert_eq!(f.app.open_modals(), vec![Modal::Terminal]);
    assert!(draw(&f.app, 100, 30).contains("done"));

    f.app.on_key(ctrl_bracket());
    assert!(!f.app.terminal_open());
    assert!(f.app.open_modals().is_empty());
    assert!(gone(&bg).await, "background {bg} survived");
}

#[tokio::test]
async fn dropping_the_app_kills_the_process_group() {
    // The quit, detach and engine-stop path: `run()` closes the terminal
    // explicitly before restoring the host terminal, and dropping the app is
    // the backstop. `event_loop` needs a real tty and engine, so the drop is
    // what a test can drive.
    let mut f = open(GROUP_STUB);
    let (leader, bg) = group_pids(&mut f).await;
    assert!(alive(&bg));
    let Fixture { app, rx: _rx, .. } = f;
    drop(app);
    assert!(gone(&leader).await, "leader {leader} survived");
    assert!(gone(&bg).await, "background {bg} survived");
}
