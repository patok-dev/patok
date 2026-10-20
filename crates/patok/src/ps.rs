//! `patok ps`: list the engine daemons running on this machine.

use std::collections::HashMap;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use patok_core::output::human_duration;
use patok_tui::{DaemonStatus, FleetEvent, RunState, TaskProgress, probe};
use serde_json::json;

/// A socket that accepts but never answers must not hang `ps`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Scans the runtime directory, probes every socket and prints the table (or the JSON array).
/// Stale and unresponsive daemons are skipped; no daemons at all still exits 0.
pub async fn run(json: bool) -> anyhow::Result<()> {
    let entries =
        patok_core::fleet::scan_from_env().context("cannot read the patok runtime directory")?;
    let probes: Vec<_> = entries
        .into_iter()
        .map(|entry| {
            tokio::spawn(async move {
                let stream = tokio::time::timeout(PROBE_TIMEOUT, probe(&entry.socket_path))
                    .await
                    .ok()?
                    .ok()?;
                Some(stream.status().clone())
            })
        })
        .collect();
    let mut statuses = Vec::new();
    for probe in probes {
        if let Ok(Some(status)) = probe.await {
            statuses.push(status);
        }
    }
    let rows = sorted_rows(statuses.iter(), now_secs());
    println!(
        "{}",
        if json {
            render_json(&rows)
        } else {
            render_table(&rows)
        }
    );
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Rows for the given daemons, ordered by project.
fn sorted_rows<'a>(statuses: impl Iterator<Item = &'a DaemonStatus>, now: u64) -> Vec<Row> {
    let mut rows: Vec<Row> = statuses.map(|status| Row::new(status, now)).collect();
    rows.sort_by(|a, b| a.project.cmp(&b.project));
    rows
}

/// One fleet change, ready to print.
enum Change {
    Added(Row),
    Changed(Row),
    Removed { project: String },
}

/// Folds a monitor event into the known fleet and returns the printable change.
fn apply(fleet: &mut HashMap<PathBuf, DaemonStatus>, event: FleetEvent, now: u64) -> Change {
    match event {
        FleetEvent::Added(status) => {
            let row = Row::new(&status, now);
            fleet.insert(status.socket_path.clone(), status);
            Change::Added(row)
        }
        FleetEvent::Changed(status) => {
            let row = Row::new(&status, now);
            fleet.insert(status.socket_path.clone(), status);
            Change::Changed(row)
        }
        FleetEvent::Removed(socket) => Change::Removed {
            project: fleet.remove(&socket).map_or_else(
                || socket.display().to_string(),
                |status| status.project_dir.display().to_string(),
            ),
        },
    }
}

fn change_line(change: &Change) -> String {
    let (word, row) = match change {
        Change::Added(row) => ("added", row),
        Change::Changed(row) => ("changed", row),
        Change::Removed { project } => return format!("removed {project}"),
    };
    format!(
        "{word} {} state={} tasks={}/{} pid={} version={}",
        row.project,
        row.state,
        row.tasks.done,
        row.tasks.total,
        row.pid.map_or_else(dash, |pid| pid.to_string()),
        if row.version.is_empty() {
            dash()
        } else {
            row.version.clone()
        },
    )
}

fn change_json(change: &Change) -> String {
    let value = match change {
        Change::Added(row) | Change::Changed(row) => {
            let mut value = row_json(row);
            let event = if matches!(change, Change::Added(_)) {
                "added"
            } else {
                "changed"
            };
            value
                .as_object_mut()
                .expect("a row is a JSON object")
                .insert("event".to_string(), json!(event));
            value
        }
        Change::Removed { project } => json!({"event": "removed", "project": project}),
    };
    serde_json::to_string(&value).expect("a JSON value always serializes")
}

/// Restores the normal screen and the cursor when dropped, however the watch ends.
struct Screen;

impl Screen {
    fn enter() -> Self {
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?1049h\x1b[?25l");
        let _ = out.flush();
        Screen
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
    }
}

/// Clears the screen and draws the current fleet as a table.
fn redraw(fleet: &HashMap<PathBuf, DaemonStatus>) -> std::io::Result<()> {
    let rows = sorted_rows(fleet.values(), now_secs());
    let mut out = std::io::stdout().lock();
    writeln!(out, "\x1b[H\x1b[2J{}", render_table(&rows))?;
    out.flush()
}

/// Prints one line and flushes it. `Ok(false)` means the reader went away (a closed pipe).
fn emit(line: &str) -> std::io::Result<bool> {
    let mut out = std::io::stdout().lock();
    match writeln!(out, "{line}").and_then(|()| out.flush()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => Err(e),
    }
}

/// `patok ps --watch`: follows the fleet until interrupted. With `--json` prints one JSON
/// object per change; otherwise redraws the table on a terminal and prints one line per
/// change when stdout is not a terminal.
pub async fn run_watch(json: bool) -> anyhow::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut fleet_monitor = patok_tui::monitor(patok_core::paths::runtime_root_from_env());
    let mut interrupt = signal(SignalKind::interrupt()).context("cannot listen for SIGINT")?;
    let mut terminate = signal(SignalKind::terminate()).context("cannot listen for SIGTERM")?;
    let tty = !json && std::io::stdout().is_terminal();
    let mut fleet: HashMap<PathBuf, DaemonStatus> = HashMap::new();
    let _screen = tty.then(Screen::enter);
    if tty {
        redraw(&fleet)?;
    }
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = interrupt.recv() => break,
            _ = terminate.recv() => break,
            _ = ticker.tick(), if tty => redraw(&fleet)?,
            event = fleet_monitor.next() => {
                let Some(event) = event else { break };
                let change = apply(&mut fleet, event, now_secs());
                if tty {
                    redraw(&fleet)?;
                } else {
                    let line = if json { change_json(&change) } else { change_line(&change) };
                    if !emit(&line)? {
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

/// One daemon's row, shared by the table and the JSON output.
struct Row {
    project: String,
    state: &'static str,
    tasks: TaskProgress,
    uptime_secs: Option<u64>,
    pid: Option<u32>,
    version: String,
}

impl Row {
    fn new(status: &DaemonStatus, now: u64) -> Self {
        Self {
            project: status.project_dir.display().to_string(),
            state: state_label(status.run_state),
            tasks: status.tasks,
            uptime_secs: status.started_at.map(|started| now.saturating_sub(started)),
            pid: status.pid,
            version: status.engine_version.clone(),
        }
    }
}

fn state_label(state: RunState) -> &'static str {
    match state {
        RunState::Idle => "idle",
        RunState::Planning => "planning",
        RunState::Running => "running",
        RunState::Discovering => "discovering",
    }
}

fn render_table(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "no running daemons".to_string();
    }
    let mut table: Vec<[String; 6]> =
        vec![["PROJECT", "STATE", "TASKS", "UPTIME", "PID", "VERSION"].map(String::from)];
    for row in rows {
        table.push([
            row.project.clone(),
            row.state.to_string(),
            format!("{}/{}", row.tasks.done, row.tasks.total),
            row.uptime_secs
                .map_or_else(dash, |secs| human_duration(Duration::from_secs(secs))),
            row.pid.map_or_else(dash, |pid| pid.to_string()),
            if row.version.is_empty() {
                dash()
            } else {
                row.version.clone()
            },
        ]);
    }
    let mut widths = [0usize; 6];
    for line in &table {
        for (width, cell) in widths.iter_mut().zip(line) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let lines: Vec<String> = table
        .iter()
        .map(|line| {
            let last = line.len() - 1;
            let mut out = String::new();
            for (i, cell) in line.iter().enumerate() {
                out.push_str(cell);
                if i < last {
                    let pad = widths[i] - cell.chars().count() + 2;
                    out.push_str(&" ".repeat(pad));
                }
            }
            out
        })
        .collect();
    lines.join("\n")
}

fn dash() -> String {
    "-".to_string()
}

fn row_json(row: &Row) -> serde_json::Value {
    json!({
        "project": row.project,
        "state": row.state,
        "tasks": {"done": row.tasks.done, "total": row.tasks.total},
        "uptime_secs": row.uptime_secs,
        "pid": row.pid,
        "version": row.version,
    })
}

fn render_json(rows: &[Row]) -> String {
    let value = serde_json::Value::Array(rows.iter().map(row_json).collect());
    serde_json::to_string_pretty(&value).expect("a JSON value always serializes")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::Value;

    use super::*;

    const NOW: u64 = 1_000_000;

    fn status(
        project: &str,
        run_state: RunState,
        (done, total): (usize, usize),
        pid: Option<u32>,
        started_at: Option<u64>,
        version: &str,
    ) -> DaemonStatus {
        DaemonStatus {
            socket_path: PathBuf::from("/run/patok/x.sock"),
            project_dir: PathBuf::from(project),
            run_state,
            tasks: TaskProgress { done, total },
            pid,
            started_at,
            engine_version: version.to_string(),
        }
    }

    fn two_rows() -> Vec<Row> {
        vec![
            Row::new(
                &status(
                    "/a",
                    RunState::Running,
                    (3, 5),
                    Some(42),
                    Some(NOW - 3900),
                    "0.1.0",
                ),
                NOW,
            ),
            Row::new(
                &status(
                    "/work/longer",
                    RunState::Idle,
                    (0, 12),
                    Some(1234),
                    Some(NOW - 12),
                    "0.1.0",
                ),
                NOW,
            ),
        ]
    }

    #[test]
    fn table_lists_rows_with_aligned_columns() {
        let expected = "\
PROJECT       STATE    TASKS  UPTIME      PID   VERSION
/a            running  3/5    1 h 05 min  42    0.1.0
/work/longer  idle     0/12   12 sec      1234  0.1.0";
        assert_eq!(render_table(&two_rows()), expected);
    }

    #[test]
    fn table_blanks_missing_pid_uptime_and_version() {
        let rows = [Row::new(
            &status("/p", RunState::Idle, (1, 2), None, None, ""),
            NOW,
        )];
        let expected = "\
PROJECT  STATE  TASKS  UPTIME  PID  VERSION
/p       idle   1/2    -       -    -";
        assert_eq!(render_table(&rows), expected);
    }

    #[test]
    fn empty_table_says_no_running_daemons() {
        assert_eq!(render_table(&[]), "no running daemons");
    }

    #[test]
    fn json_lists_the_same_fields() {
        let mut rows = two_rows();
        rows.push(Row::new(
            &status("/z", RunState::Planning, (2, 2), None, None, ""),
            NOW,
        ));
        let value: Value = serde_json::from_str(&render_json(&rows)).unwrap();
        let array = value.as_array().unwrap();
        assert_eq!(array.len(), 3);
        assert_eq!(array[0]["project"], "/a");
        assert_eq!(array[0]["state"], "running");
        assert_eq!(array[0]["tasks"]["done"], 3);
        assert_eq!(array[0]["tasks"]["total"], 5);
        assert_eq!(array[0]["uptime_secs"], 3900);
        assert_eq!(array[0]["pid"], 42);
        assert_eq!(array[0]["version"], "0.1.0");
        assert_eq!(array[1]["uptime_secs"], 12);
        assert_eq!(array[1]["pid"], 1234);
        assert_eq!(array[2]["state"], "planning");
        assert!(array[2]["pid"].is_null());
        assert!(array[2]["uptime_secs"].is_null());
    }

    #[test]
    fn empty_json_is_an_empty_array() {
        let value: Value = serde_json::from_str(&render_json(&[])).unwrap();
        assert_eq!(value, Value::Array(Vec::new()));
    }

    #[test]
    fn state_labels() {
        assert_eq!(state_label(RunState::Idle), "idle");
        assert_eq!(state_label(RunState::Planning), "planning");
        assert_eq!(state_label(RunState::Running), "running");
        assert_eq!(state_label(RunState::Discovering), "discovering");
    }

    #[test]
    fn uptime_never_negative() {
        let row = Row::new(
            &status("/p", RunState::Idle, (0, 0), None, Some(NOW + 50), ""),
            NOW,
        );
        assert_eq!(row.uptime_secs, Some(0));
    }

    fn at(socket: &str, project: &str, state: RunState, pid: Option<u32>, v: &str) -> DaemonStatus {
        let mut s = status(project, state, (3, 5), pid, Some(NOW - 7), v);
        s.socket_path = PathBuf::from(socket);
        s
    }

    #[test]
    fn change_line_formats_added_and_changed() {
        let added = Change::Added(Row::new(
            &status("/a", RunState::Running, (3, 5), Some(42), None, "0.1.0"),
            NOW,
        ));
        let changed = Change::Changed(Row::new(
            &status("/a", RunState::Idle, (5, 5), Some(42), None, "0.1.0"),
            NOW,
        ));
        assert_eq!(
            change_line(&added),
            "added /a state=running tasks=3/5 pid=42 version=0.1.0"
        );
        assert_eq!(
            change_line(&changed),
            "changed /a state=idle tasks=5/5 pid=42 version=0.1.0"
        );
    }

    #[test]
    fn change_line_blanks_missing_pid_and_version() {
        let change = Change::Added(Row::new(
            &status("/p", RunState::Idle, (1, 2), None, None, ""),
            NOW,
        ));
        assert_eq!(
            change_line(&change),
            "added /p state=idle tasks=1/2 pid=- version=-"
        );
    }

    #[test]
    fn change_line_formats_removed() {
        let change = Change::Removed {
            project: "/a".to_string(),
        };
        assert_eq!(change_line(&change), "removed /a");
    }

    #[test]
    fn change_json_is_one_compact_object_per_change() {
        let full = Change::Added(Row::new(
            &status(
                "/a",
                RunState::Running,
                (3, 5),
                Some(42),
                Some(NOW - 9),
                "0.1.0",
            ),
            NOW,
        ));
        let text = change_json(&full);
        assert!(!text.contains('\n'));
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["event"], "added");
        assert_eq!(v["project"], "/a");
        assert_eq!(v["state"], "running");
        assert_eq!(v["tasks"]["done"], 3);
        assert_eq!(v["tasks"]["total"], 5);
        assert_eq!(v["uptime_secs"], 9);
        assert_eq!(v["pid"], 42);
        assert_eq!(v["version"], "0.1.0");

        let bare = Change::Changed(Row::new(
            &status("/p", RunState::Idle, (1, 2), None, None, ""),
            NOW,
        ));
        let v: Value = serde_json::from_str(&change_json(&bare)).unwrap();
        assert_eq!(v["event"], "changed");
        assert!(v["pid"].is_null());
        assert!(v["uptime_secs"].is_null());
    }

    #[test]
    fn change_json_removed_has_event_and_project() {
        let change = Change::Removed {
            project: "/a".to_string(),
        };
        let v: Value = serde_json::from_str(&change_json(&change)).unwrap();
        assert_eq!(v, json!({"event": "removed", "project": "/a"}));
    }

    #[test]
    fn apply_tracks_fleet_and_names_removed_daemons() {
        let mut fleet = HashMap::new();
        let added = apply(
            &mut fleet,
            FleetEvent::Added(at("/s1", "/a", RunState::Running, Some(1), "v")),
            NOW,
        );
        assert!(matches!(added, Change::Added(_)));
        assert_eq!(fleet.len(), 1);
        let changed = apply(
            &mut fleet,
            FleetEvent::Changed(at("/s1", "/a", RunState::Idle, Some(1), "v")),
            NOW,
        );
        assert!(matches!(changed, Change::Changed(_)));
        assert_eq!(fleet.len(), 1);
        assert_eq!(fleet[&PathBuf::from("/s1")].run_state, RunState::Idle);
        let removed = apply(&mut fleet, FleetEvent::Removed(PathBuf::from("/s1")), NOW);
        assert_eq!(change_line(&removed), "removed /a");
        assert!(fleet.is_empty());
        let unknown = apply(
            &mut fleet,
            FleetEvent::Removed(PathBuf::from("/unknown.sock")),
            NOW,
        );
        assert_eq!(change_line(&unknown), "removed /unknown.sock");
    }
}
