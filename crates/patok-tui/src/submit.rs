//! The submit policy (finding 19, the rest, in
//! docs/private/structure-review-followup.md): the add-task and
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

    /// The Enter key (T46.1): while the engine is idle it starts the build
    /// loop when pending tasks remain; in the NeedsQueue scenario it
    /// bootstraps the queue through the research agent (moved here from the
    /// add dialog's empty submit, T5.5; it still requires spec content);
    /// otherwise it runs a discovery round. The add dialog's former Start and
    /// Scan empty submits are covered by the StartBuild and RunDiscovery arms
    /// (T5.5). A busy engine refuses with the same short message as the other
    /// keys.
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
                } else if self.scenario() == Scenario::NeedsQueue {
                    if self.project_status.spec == SpecState::Content {
                        Action::ResearchQueue(QueueRun::Bootstrap)
                    } else {
                        self.status = Some(
                            "SPEC.md is empty -- describe what you want to build first.".into(),
                        );
                        Action::None
                    }
                } else {
                    Action::RunDiscovery
                }
            }
        }
    }

    /// The inline refusal for an empty input on the dialog's primary key:
    /// kind-dependent, because the add and edit dialogs submit different things.
    pub(crate) fn empty_input_message(&self) -> &'static str {
        match self.dialog.kind {
            DialogKind::Add => "Nothing to submit -- type a request first.",
            DialogKind::Edit => "Nothing to save -- type the task description first.",
        }
    }

    /// Whether the dialog's confirm is active: the add dialog's is inactive
    /// while the text is blank (T5.2); the edit kind refuses inline instead.
    pub(crate) fn confirm_enabled(&self) -> bool {
        !(self.dialog.kind == DialogKind::Add && self.dialog.editor.text().trim().is_empty())
    }

    /// The add dialog only adds tasks: unchecked or busy -> `InjectTask`
    /// (lines collapsed into one), checked and idle -> `SubmitTasks`
    /// (verbatim to the planner). Blank text is refused for Enter, Ctrl+S and
    /// the button alike. The scenario-driven empty submit was removed in T5.5:
    /// start and discovery are main-screen Enter (`enter_key`), the NeedsQueue
    /// bootstrap moved there too, the QueueComplete gap scan was dropped in
    /// favour of Enter's discovery round, and the EmptyProject brief
    /// (`SaveBrief`) is gone -- SPEC.md is written by hand or through the
    /// agent add.
    pub(crate) fn submit_dialog(&mut self) -> Action {
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
        let text = self.dialog.editor.text();
        if text.trim().is_empty() {
            self.dialog.status = Some(self.empty_input_message().into());
            return Action::None;
        }
        if self.effective_use_agent() {
            Action::SubmitTasks(text.to_owned())
        } else {
            Action::InjectTask(collapse_lines(text))
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

    /// The add dialog's plain add was appended to TASKS.md (`InjectTask`,
    /// T5.2; the former inject modal's completion, folded in by T5.4): the
    /// dialog closes, the input clears and the task list frame keeps the
    /// focus, where the engine's task-file poll brings the new line in.
    pub fn on_task_added(&mut self) {
        self.modals.pop(Modal::Dialog);
        self.dialog.editor.clear();
        self.dialog.status = None;
        self.focus = FrameFocus::Tasks;
    }

    /// Closes the dialog (Esc, the close button), putting back the draft the
    /// edit dialog parked; the add buffer is otherwise kept.
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
    use patok_core::scenario::{ProjectScan, TaskFileState};

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
    fn busy_blank_submit_is_refused() {
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

    fn scan(has_code: bool, task_file: TaskFileState, spec: SpecState) -> ProjectScan {
        ProjectScan {
            has_code,
            task_file,
            spec,
        }
    }

    #[test]
    fn ctrl_s_on_blank_add_is_refused_in_every_scenario() {
        let cases = [
            (
                vec![],
                scan(false, TaskFileState::Missing, SpecState::Missing),
            ),
            (
                vec![],
                scan(true, TaskFileState::Missing, SpecState::Content),
            ),
            (
                patok_core::task::parse("- [ ] T1.1: x\n"),
                scan(true, TaskFileState::Ok, SpecState::Content),
            ),
            (
                patok_core::task::parse("- [x] T1.1: x\n"),
                scan(true, TaskFileState::Ok, SpecState::Content),
            ),
        ];
        for (tasks, status) in cases {
            let mut app = app();
            app.run.tasks = tasks;
            app.project_status = status;
            app.open_dialog();
            let action = app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
            assert_eq!(action, Action::None);
            assert!(app.modals.contains(Modal::Dialog));
            assert_eq!(
                app.dialog.status.as_deref(),
                Some(app.empty_input_message())
            );
        }
    }

    #[test]
    fn enter_key_bootstraps_a_needs_queue_project_with_spec_content() {
        let mut app = app();
        app.run.tasks = vec![];
        app.project_status = scan(true, TaskFileState::Missing, SpecState::Content);
        assert_eq!(app.enter_key(), Action::ResearchQueue(QueueRun::Bootstrap));
        assert!(app.status.is_none());
        for spec in [SpecState::Empty, SpecState::Missing] {
            app.project_status = scan(true, TaskFileState::Missing, spec);
            assert_eq!(app.enter_key(), Action::None);
            assert_eq!(
                app.status.as_deref(),
                Some("SPEC.md is empty -- describe what you want to build first.")
            );
        }
    }

    #[test]
    fn enter_key_keeps_start_and_discovery_outside_needs_queue() {
        let mut app = app();
        app.project_status = scan(true, TaskFileState::Missing, SpecState::Content);
        assert_eq!(app.enter_key(), Action::StartBuild);
        app.run.tasks = patok_core::task::parse("- [x] T1.1: x\n");
        app.project_status = scan(true, TaskFileState::Ok, SpecState::Content);
        assert_eq!(app.enter_key(), Action::RunDiscovery);
        app.run.tasks = vec![];
        app.project_status = ProjectScan::default();
        assert_eq!(app.enter_key(), Action::RunDiscovery);
    }
}
