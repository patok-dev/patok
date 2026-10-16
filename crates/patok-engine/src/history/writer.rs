//! History writer for session and task logging.
//!
//! This module implements the `HistoryWriter` that consumes `EngineEvent`s and
//! session/task markers from an unbounded mpsc channel, uses `Pane` merging to
//! turn streamed deltas into lines, and writes `output.log` (all lines),
//! `orchestrator.log` (orchestrator-origin lines only), and `events.jsonl`
//! (raw events) to the active task directory when a task is open, otherwise
//! to the session directory.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use patok_core::event::EngineEvent;
use patok_core::output::{LineOrigin, OutLine, Pane};
use tokio::sync::mpsc;

/// Messages sent to the history writer.
#[derive(Debug, Clone)]
pub enum HistoryMsg {
    /// An engine event to process.
    Event(EngineEvent),
    /// A session has opened.
    SessionOpen { path: PathBuf },
    /// A session has closed.
    SessionClose,
    /// A task has opened.
    TaskOpen { path: PathBuf },
    /// A task has closed.
    TaskClose,
}

/// The history writer: a tokio task that consumes history messages and writes
/// log files to the appropriate session or task directory.
pub struct HistoryWriter {
    /// Receiver for history messages.
    receiver: mpsc::UnboundedReceiver<HistoryMsg>,
    /// Current session path.
    session_path: Option<PathBuf>,
    /// Current task path (when inside a task).
    task_path: Option<PathBuf>,
    /// Pane for accumulating output lines.
    output_pane: Pane,
    /// Pane for accumulating orchestrator-only lines.
    orchestrator_pane: Pane,
}

impl HistoryWriter {
    /// Creates a new HistoryWriter with the given receiver.
    pub fn new(receiver: mpsc::UnboundedReceiver<HistoryMsg>) -> Self {
        Self {
            receiver,
            session_path: None,
            task_path: None,
            output_pane: Pane::default(),
            orchestrator_pane: Pane::default(),
        }
    }

    /// Creates a new HistoryWriter with its own channel.
    #[allow(dead_code)]
    pub fn with_channel() -> (Self, mpsc::UnboundedSender<HistoryMsg>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (Self::new(receiver), sender)
    }

    /// Returns the current directory for writing files: the task directory if
    /// a task is open, otherwise the session directory.
    fn current_dir(&self) -> Option<&Path> {
        self.task_path.as_deref().or(self.session_path.as_deref())
    }

    /// Processes an event by applying it to both panes and writing lines to disk.
    fn process_event(&mut self, event: EngineEvent) {
        // Apply the event to both panes
        let open = self.output_pane.is_open();
        let event_lines = patok_core::output::event_lines(&event, open);

        // Update output pane
        for line in &event_lines.lines {
            self.output_pane.push(line.clone());
            // Write orchestrator lines to orchestrator pane too
            if line.origin == LineOrigin::Orchestrator {
                self.orchestrator_pane.push(line.clone());
            }
        }

        // Handle append for output pane
        if let Some(chunk) = &event_lines.append {
            self.output_pane.append(LineOrigin::Agent, chunk);
            // Also append to orchestrator pane if it's from orchestrator
            // Note: TextDelta events are always from Agent, so we don't need to check
        }

        // Flush lines to disk
        self.write_lines();

        // Write raw event to events.jsonl
        self.write_event(&event);
    }

    /// Writes accumulated lines from both panes to their respective log files.
    fn write_lines(&self) {
        let Some(dir) = self.current_dir() else {
            return;
        };

        // Write output.log
        if !self.output_pane.lines.is_empty() {
            let path = dir.join("output.log");
            let content = Self::format_lines(&self.output_pane.lines);
            let _ = std::fs::write(&path, content);
        }

        // Write orchestrator.log
        if !self.orchestrator_pane.lines.is_empty() {
            let path = dir.join("orchestrator.log");
            let content = Self::format_lines(&self.orchestrator_pane.lines);
            let _ = std::fs::write(&path, content);
        }
    }

    /// Formats a collection of lines as a single string with newlines.
    fn format_lines(lines: &VecDeque<OutLine>) -> String {
        lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Writes a raw event to events.jsonl.
    fn write_event(&self, event: &EngineEvent) {
        let Some(dir) = self.current_dir() else {
            return;
        };
        let path = dir.join("events.jsonl");
        let json = serde_json::to_string(event).unwrap_or_default();
        // Append to file
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = std::io::Write::write_all(&mut file, json.as_bytes());
            let _ = std::io::Write::write_all(&mut file, b"\n");
        }
    }

    /// Handles a SessionOpen message.
    fn handle_session_open(&mut self, path: PathBuf) {
        self.session_path = Some(path);
        self.task_path = None;
        // Reset panes when session opens
        self.output_pane = Pane::default();
        self.orchestrator_pane = Pane::default();
    }

    /// Handles a SessionClose message.
    fn handle_session_close(&mut self) {
        self.session_path = None;
        self.task_path = None;
    }

    /// Handles a TaskOpen message.
    fn handle_task_open(&mut self, path: PathBuf) {
        self.task_path = Some(path);
    }

    /// Handles a TaskClose message.
    fn handle_task_close(&mut self) {
        self.task_path = None;
    }

    /// The main async run loop for the writer.
    pub async fn run(mut self) {
        while let Some(msg) = self.receiver.recv().await {
            match msg {
                HistoryMsg::Event(event) => {
                    self.process_event(event);
                }
                HistoryMsg::SessionOpen { path } => {
                    self.handle_session_open(path);
                }
                HistoryMsg::SessionClose => {
                    self.handle_session_close();
                }
                HistoryMsg::TaskOpen { path } => {
                    self.handle_task_open(path);
                }
                HistoryMsg::TaskClose => {
                    self.handle_task_close();
                }
            }
        }
    }
}
