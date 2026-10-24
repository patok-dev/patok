//! The submit policy (finding 19, the rest, in
//! docs/private/structure-review-followup.md): the add-task, inject-task and
//! edit-task (T4.3) dialog's open, refuse and submit paths and the driver's submit
//! callbacks, split out of `app`. The editor itself is `dialog_editor`.

use patok_core::event::Phase;
use patok_core::scenario::{Scenario, SpecState};

use crate::app::{Action, App, DialogKind, QueueRun};
use crate::modal::Modal;
use crate::view::FrameFocus;

/// The add dialog's plain-add text (T5.2): every line trimmed, blank lines
/// dropped, the rest joined with one space -- a task line is single-line.
pub(crate) fn collapse_lines(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

impl App {
    /// The short inline refusal for a busy engine, shared by Enter and the add
    /// dialog's agent gate (`agent_available`): `None` while the engine is idle.
    pub(crate) fn busy_status(&self) -> Option<&'static str> {
        if self.run.phase == Phase::Running {
            Some("A build is running; wait for it to finish.")
        } else if self.run.planning {
            Some("The planner is already running.")
        } else if self.run.discovering {
            Some("A discovery round is running; wait for it to finish.")
        } else {
            None
        }
    }

    /// The add dialog's agent path is idle-only (T5.3): while the engine is busy
    /// the "Use agent to add" box is forced unchecked and disabled.
    pub(crate) fn agent_available(&self) -> bool {
        self.busy_status().is_none()
    }

    /// The checkbox state submit and render act on: the stored choice, forced
    /// off while the engine is busy.
    pub(crate) fn effective_use_agent(&self) -> bool {
        self.dialog.use_agent && self.agent_available()
    }

    /// Opens the add-task dialog in any engine state; a busy engine only
    /// disables the "Use agent to add" checkbox (T5.3).
    pub(crate) fn open_dialog(&mut self) -> Action {
        self.status = None;
        self.dialog.kind = DialogKind::Add;
        if self.modals.push(Modal::Dialog) {
            self.dialog.status = None;
        }
        Action::None
    }

    /// Opens the inject-task modal (the `i` key, T76.1) from any engine state:
    /// the shared input box with the inject kind, whose confirm sends the
    /// text to the engine, which normalizes it into a well-formed unchecked
    /// task line before appending it. No busy state refuses it -- the append
    /// is a plain file write, so the engine's task-file poll reconciles the
    /// line even while a run is active. A refused push (another modal
    /// already open) is a no-op: the kind and the status are left alone.
    pub(crate) fn open_inject(&mut self) -> Action {
        self.status = None;
        if self.modals.push(Modal::Dialog) {
            self.dialog.kind = DialogKind::Inject;
            self.dialog.status = None;
        }
        Action::None
    }

    /// The Enter key (T46.1): while the engine is idle it starts the build
    /// loop when pending tasks remain and runs a discovery round when the
    /// queue is complete; a busy engine refuses with the same short message
    /// as the other keys.
    pub(crate) fn enter_key(&mut self) -> Action {
        self.status = None;
        match self.busy_status() {
            Some(message) => {
                self.status = Some(message.into());
                Action::None
            }
            None => {
                if self.run.tasks.iter().any(|t| !t.done) {
                    Action::StartBuild
                } else {
                    Action::RunDiscovery
                }
            }
        }
    }

    /// The inline refusal for an empty input on the dialog's primary key:
    /// kind-dependent (T76.1), because the two modals submit different things.
    pub(crate) fn empty_input_message(&self) -> &'static str {
        match self.dialog.kind {
            DialogKind::Add => "Nothing to submit -- type a request first.",
            DialogKind::Inject => "Nothing to inject -- type a task line first.",
            DialogKind::Edit => "Nothing to save -- type the task description first.",
        }
    }

    /// Whether the dialog's confirm is active: the add dialog's is inactive
    /// while the text is blank (T5.2); the other kinds refuse inline instead.
    pub(crate) fn confirm_enabled(&self) -> bool {
        !(self.dialog.kind == DialogKind::Add && self.dialog.editor.text().trim().is_empty())
    }

    /// The scenario-driven submit: what submitting does depends
    /// on the project scenario and whether the input is empty. Non-empty text
    /// is a plain append (`InjectTask`, lines collapsed into one) while the
    /// "Use agent to add" box is unchecked and goes verbatim to the planner
    /// (`SubmitTasks`) when checked. A busy engine (T5.3) offers only the plain
    /// append: the scenario flows are skipped and blank text is refused. A submit that launches
    /// a run or the build loop closes the dialog; one that only writes the spec file
    /// or refuses keeps it open with its message in the status line.
    pub(crate) fn submit_dialog(&mut self) -> Action {
        // The inject-task modal's submit (T76.1): the typed text goes to the
        // engine, which normalizes it into a well-formed unchecked task line
        // and appends it under its task-file lock -- never to an agent
        // session; the modal closes once the driver confirms the append
        // (`on_task_injected`). An empty input is refused inline, and the
        // add-task dialog's whole scenario flow below stays untouched.
        // The edit dialog's submit (T4.3): the trimmed text goes to the engine's
        // edit command, which also rejects multi-line text and busy tasks; the
        // dialog closes once the driver confirms (`on_task_edited`).
        if self.dialog.kind == DialogKind::Edit {
            let text = self.dialog.editor.text().trim().to_owned();
            if text.is_empty() {
                self.dialog.status = Some(self.empty_input_message().into());
                return Action::None;
            }
            return Action::EditTask {
                id: self.dialog.edit_id.clone(),
                description: text,
            };
        }
        if self.dialog.kind == DialogKind::Inject {
            if self.dialog.editor.text().trim().is_empty() {
                self.dialog.status = Some(self.empty_input_message().into());
                return Action::None;
            }
            return Action::InjectTask(self.dialog.editor.text().to_owned());
        }
        let empty = self.dialog.editor.text().trim().is_empty();
        if !self.agent_available() {
            // A busy engine (T5.3): only the plain append is offered. The
            // scenario flows below (brief, bootstrap, start, scan) all start
            // something, so they are skipped; blank text is refused inline.
            if empty {
                self.dialog.status = Some(self.empty_input_message().into());
                return Action::None;
            }
            return Action::InjectTask(collapse_lines(self.dialog.editor.text()));
        }
        match (self.scenario(), empty) {
            (Scenario::EmptyProject, false) => {
                Action::SaveBrief(self.dialog.editor.text().to_owned())
            }
            (Scenario::EmptyProject, true) => {
                self.dialog.status = Some(
                    "Describe what you want to build -- an empty project needs direction.".into(),
                );
                Action::None
            }
            (Scenario::NeedsQueue, true) => {
                if self.project_status.spec == SpecState::Content {
                    Action::ResearchQueue(QueueRun::Bootstrap)
                } else {
                    self.dialog.status =
                        Some("SPEC.md is empty -- describe what you want to build first.".into());
                    Action::None
                }
            }
            (Scenario::QueueReady, true) => {
                self.modals.pop(Modal::Dialog);
                self.focus = FrameFocus::Output;
                Action::StartBuild
            }
            (Scenario::QueueComplete, true) => Action::ResearchQueue(QueueRun::Scan),
            (_, false) if self.effective_use_agent() => {
                Action::SubmitTasks(self.dialog.editor.text().to_owned())
            }
            (_, false) => Action::InjectTask(collapse_lines(self.dialog.editor.text())),
        }
    }

    /// The engine accepted the request: the dialog closes immediately and the output
    /// frame takes the focus, where the planner's output streams (T24.1).
    pub fn on_tasks_submitted(&mut self) {
        self.modals.pop(Modal::Dialog);
        self.dialog.editor.clear();
        self.dialog.status = None;
        self.focus = FrameFocus::Output;
    }

    /// The injected task line was appended to TASKS.md (T76.1): the modal
    /// closes and the task list frame keeps the focus, where the engine's
    /// task-file poll brings the new line in as a TasksChanged event.
    pub fn on_task_injected(&mut self) {
        self.modals.pop(Modal::Dialog);
        self.dialog.editor.clear();
        self.dialog.status = None;
        self.focus = FrameFocus::Tasks;
    }

    /// Closes the dialog (Esc, the close button), putting back the draft the
    /// edit dialog parked; the add/inject buffer is otherwise kept.
    pub(crate) fn close_dialog(&mut self) {
        self.modals.pop(Modal::Dialog);
        self.restore_parked();
    }

    fn restore_parked(&mut self) {
        if let Some(editor) = self.dialog.parked.take() {
            self.dialog.editor = editor;
        }
    }

    /// The edit was applied (T4.3): the dialog closes, the parked draft comes
    /// back and the task list frame keeps the focus.
    pub fn on_task_edited(&mut self) {
        self.modals.pop(Modal::Dialog);
        self.restore_parked();
        self.dialog.status = None;
        self.focus = FrameFocus::Tasks;
    }

    /// The command failed: the dialog stays open on the input with the error.
    pub fn on_submit_failed(&mut self, error: String) {
        self.dialog.status = Some(error);
    }

    /// The brief was written to SPEC.md (EmptyProject submit): the text moved into the
    /// file, so the input clears and the dialog stays open with the created message;
    /// with the spec file now present the scenario re-detects as NeedsQueue.
    pub fn on_brief_saved(&mut self) {
        self.dialog.editor.clear();
        self.dialog.status =
            Some("SPEC.md created -- review it, then press Enter to start.".into());
    }

    /// Pasted text goes to the terminal when it is the top modal, else appends
    /// to the dialog's input; ignored while neither is open.
    pub fn on_paste(&mut self, text: &str) {
        if self.modals.top() == Some(Modal::Terminal) {
            self.terminal_paste(text);
        } else if self.modals.contains(Modal::Dialog) {
            self.dialog.editor.insert_paste(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_core::event::{Phase, Snapshot};
    use patok_core::pipeline::PipelineState;

    use super::*;

    /// An idle app with one pending task, so the scenario is `QueueReady` and
    /// a non-empty submit reaches the plain-add / agent routing.
    fn app() -> App {
        let mut app = App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: vec![],
                current_task: None,
                planning: false,
                discovering: false,
                provider: "claude".into(),
                model: String::new(),
                settings: BTreeMap::new(),
                pipeline: PipelineState::today(),
                model_catalog: Vec::new(),
                recent: vec![],
            },
            "0.1.0".into(),
        );
        app.run.tasks = patok_core::task::parse("- [ ] T1.1: x\n");
        app
    }

    fn enter(app: &mut App) -> Action {
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    }

    #[test]
    fn collapse_lines_joins_trimmed_non_blank_lines() {
        assert_eq!(collapse_lines("  a \n\n b\t\n"), "a b");
        assert_eq!(collapse_lines("single"), "single");
        assert_eq!(collapse_lines("x  y"), "x  y");
        assert_eq!(collapse_lines("\n \n"), "");
    }

    #[test]
    fn unchecked_submit_sends_inject_task_collapsed() {
        let mut app = app();
        app.open_dialog();
        app.dialog
            .editor
            .insert_paste("first line\n\n  second line ");
        app.dialog.use_agent = false;
        assert_eq!(
            app.submit_dialog(),
            Action::InjectTask("first line second line".into())
        );
    }

    #[test]
    fn checked_submit_sends_submit_tasks_verbatim() {
        let mut app = app();
        app.open_dialog();
        app.dialog
            .editor
            .insert_paste("first line\n\n  second line ");
        app.dialog.use_agent = true;
        assert_eq!(
            app.submit_dialog(),
            Action::SubmitTasks("first line\n\n  second line ".into())
        );
    }

    #[test]
    fn confirm_enabled_tracks_blank_text_for_add_only() {
        let mut app = app();
        app.open_dialog();
        assert!(!app.confirm_enabled());
        app.dialog.editor.insert_paste("  \n ");
        assert!(!app.confirm_enabled());
        app.dialog.editor.clear();
        app.dialog.editor.insert_paste("x");
        assert!(app.confirm_enabled());

        let mut inject = self::app();
        inject.open_inject();
        assert!(inject.confirm_enabled());

        let mut edit = self::app();
        edit.edit_selected_task();
        edit.dialog.editor.clear();
        assert_eq!(edit.dialog.kind, DialogKind::Edit);
        assert!(edit.confirm_enabled());
    }

    #[test]
    fn enter_on_blank_add_is_refused_whatever_the_checkbox() {
        for use_agent in [false, true] {
            let mut app = app();
            app.open_dialog();
            app.dialog.use_agent = use_agent;
            assert_eq!(enter(&mut app), Action::None);
            assert!(app.modals.contains(Modal::Dialog));
            assert_eq!(
                app.dialog.status.as_deref(),
                Some(app.empty_input_message())
            );
        }
    }

    /// Runs `check` over a fresh app in each busy engine state (T5.3).
    fn busy(check: impl Fn(&mut App)) {
        let setups: [fn(&mut App); 3] = [
            |a| a.run.phase = Phase::Running,
            |a| a.run.planning = true,
            |a| a.run.discovering = true,
        ];
        for setup in setups {
            let mut app = app();
            setup(&mut app);
            check(&mut app);
        }
    }

    #[test]
    fn open_dialog_opens_while_busy() {
        busy(|app| {
            app.open_dialog();
            assert!(app.modals.contains(Modal::Dialog));
            assert!(app.status.is_none());
            assert_eq!(app.dialog.kind, DialogKind::Add);
        });
    }

    #[test]
    fn busy_submit_is_plain_add_even_when_checked() {
        busy(|app| {
            app.open_dialog();
            app.dialog.use_agent = true;
            app.dialog.editor.insert_paste("first\n\n second ");
            assert_eq!(
                app.submit_dialog(),
                Action::InjectTask("first second".into())
            );
            assert!(!app.effective_use_agent());
            assert!(app.dialog.use_agent, "the preference is kept");
        });
    }

    #[test]
    fn busy_blank_submit_is_refused_not_scenario() {
        busy(|app| {
            app.open_dialog();
            assert_eq!(app.submit_dialog(), Action::None);
            assert_eq!(
                app.dialog.status.as_deref(),
                Some(app.empty_input_message())
            );
            assert!(app.modals.contains(Modal::Dialog));
        });
    }

    #[test]
    fn toggle_is_ignored_while_busy() {
        busy(|app| {
            app.open_dialog();
            app.toggle_use_agent();
            assert!(!app.dialog.use_agent);
            app.dialog.use_agent = true;
            app.toggle_use_agent();
            assert!(app.dialog.use_agent);
        });
    }

    #[test]
    fn agent_path_returns_when_idle() {
        let mut app = app();
        app.run.planning = true;
        app.open_dialog();
        app.dialog.use_agent = true;
        app.dialog.editor.insert_paste("plan this");
        app.run.planning = false;
        assert_eq!(app.submit_dialog(), Action::SubmitTasks("plan this".into()));
    }

    #[test]
    fn primary_label_is_submit_while_busy() {
        busy(|app| {
            app.open_dialog();
            assert_eq!(app.primary_label(), "Submit");
        });
    }
}
