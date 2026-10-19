//! `patok ps`: list the engine daemons running on this machine.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use patok_core::output::human_duration;
use patok_tui::{DaemonStatus, RunState, TaskProgress, probe};
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
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut rows = Vec::new();
    for probe in probes {
        if let Ok(Some(status)) = probe.await {
            rows.push(Row::new(&status, now));
        }
    }
    rows.sort_by(|a, b| a.project.cmp(&b.project));
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

fn render_json(rows: &[Row]) -> String {
    let value = serde_json::Value::Array(
        rows.iter()
            .map(|row| {
                json!({
                    "project": row.project,
                    "state": row.state,
                    "tasks": {"done": row.tasks.done, "total": row.tasks.total},
                    "uptime_secs": row.uptime_secs,
                    "pid": row.pid,
                    "version": row.version,
                })
            })
            .collect(),
    );
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
}
