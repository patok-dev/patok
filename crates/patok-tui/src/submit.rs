//! The submit policy (finding 19, the rest, in
//! docs/private/structure-review-followup.md): the add-task and inject-task
//! dialog's open, refuse and submit paths and the driver's submit
//! callbacks, split out of `app`. The editor itself is `dialog_editor`.

use patok_core::event::Phase;
use patok_core::scenario::{Scenario, SpecState};

use crate::app::{Action, App, DialogKind, QueueRun};
use crate::modal::Modal;
use crate::view::FrameFocus;

impl App {
    /// The short inline refusal for a busy engine, shared by every key that
    /// starts something (Enter, `a`): `None` while the engine is idle.
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

    /// Opens the add-task dialog when the engine is idle; a busy engine refuses with a
    /// short inline message (the same message the Enter key shows).
    pub(crate) fn open_dialog(&mut self) -> Action {
        self.status = None;
        self.dialog.kind = DialogKind::Add;
        match self.busy_status() {
            Some(message) => self.status = Some(message.into()),
            None => {
                if self.modals.push(Modal::Dialog) {
                    self.dialog.status = None;
                }
            }
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
        }
    }

    /// The scenario-driven submit: what submitting does depends
    /// on the project scenario and whether the input is empty. A submit that launches
    /// a run or the build loop closes the dialog; one that only writes the spec file
    /// or refuses keeps it open with its message in the status line.
    pub(crate) fn submit_dialog(&mut self) -> Action {
        // The inject-task modal's submit (T76.1): the typed text goes to the
        // engine, which normalizes it into a well-formed unchecked task line
        // and appends it under its task-file lock -- never to an agent
        // session; the modal closes once the driver confirms the append
        // (`on_task_injected`). An empty input is refused inline, and the
        // add-task dialog's whole scenario flow below stays untouched.
        if self.dialog.kind == DialogKind::Inject {
            if self.dialog.editor.text().trim().is_empty() {
                self.dialog.status = Some(self.empty_input_message().into());
                return Action::None;
            }
            return Action::InjectTask(self.dialog.editor.text().to_owned());
        }
        let empty = self.dialog.editor.text().trim().is_empty();
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
            (_, false) => Action::SubmitTasks(self.dialog.editor.text().to_owned()),
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

    /// Pasted text appends to the dialog's input; ignored while the dialog is closed.
    pub fn on_paste(&mut self, text: &str) {
        if self.modals.contains(Modal::Dialog) {
            self.dialog.editor.insert_paste(text);
        }
    }
}
