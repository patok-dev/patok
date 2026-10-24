//! The task list's edit actions (T4.2, T4.3): the selected task's delete (behind
//! a confirmation dialog, T7.2), move and indent/outdent keys and the `e` edit
//! dialog. The key side refuses done, running and malformed rows
//! with a status message; the driver side moves the cursor after the engine
//! accepts, using the engine's own placement rules.

use crate::app::{Action, App, DialogKind, TaskChange};
use crate::modal::Modal;

impl App {
    /// The action for a change of the selected task, or `Action::None` with a
    /// status message when the row cannot be changed.
    pub(crate) fn change_selected_task(&mut self, change: TaskChange) -> Action {
        match self.changeable_task(change.verb()) {
            Some(i) => Action::ChangeTask {
                id: self.run.tasks[i].id.clone(),
                change,
            },
            None => Action::None,
        }
    }

    /// The index of the selected task when it may be changed; otherwise sets
    /// the refusal status (no task, malformed, done, running; `verb` names the
    /// refused change) and returns `None`. A success clears the status.
    pub(crate) fn changeable_task(&mut self, verb: &str) -> Option<usize> {
        let Some(i) = self.selected_task_index() else {
            self.status = Some("No task to change.".into());
            return None;
        };
        if let Some(message) = self.refusal(i, verb) {
            self.status = Some(message);
            return None;
        }
        self.status = None;
        Some(i)
    }

    /// Why task `i` cannot be changed (malformed, done or running; `verb`
    /// names the refused change), or `None` when it can.
    fn refusal(&self, i: usize, verb: &str) -> Option<String> {
        let task = &self.run.tasks[i];
        let id = &task.id;
        if task.is_malformed() {
            Some("This task line has no valid id; fix it in TASKS.md first.".to_string())
        } else if task.done {
            Some(format!("Task {id} is completed and cannot be {verb}."))
        } else if self.run.current_task.as_deref() == Some(id.as_str()) {
            Some(format!("Task {id} is in progress and cannot be {verb}."))
        } else {
            None
        }
    }

    /// The `d` key (T7.2): opens the delete confirmation for the selected
    /// task. A refused row (none, malformed, done, running) opens no dialog
    /// and sets the usual status; the removal itself waits for the dialog.
    pub(crate) fn delete_selected_task(&mut self) -> Action {
        let Some(i) = self.changeable_task(TaskChange::Remove.verb()) else {
            return Action::None;
        };
        let id = self.run.tasks[i].id.clone();
        self.open_delete(id);
        Action::None
    }

    /// Opens the delete dialog for `id`. A refused push is a no-op that
    /// leaves the dialog's state alone.
    pub(crate) fn open_delete(&mut self, id: String) {
        if self.modals.push(Modal::Delete) {
            self.delete.id = id;
            self.delete.selected = 0;
        }
    }

    /// The delete dialog's Delete choice (T7.2): the task list may have
    /// changed while the dialog was open, so the pending id is looked up and
    /// checked again before the remove action is returned.
    pub(crate) fn confirm_delete(&mut self) -> Action {
        let id = std::mem::take(&mut self.delete.id);
        let Some(i) = self.run.tasks.iter().position(|t| t.id == id) else {
            self.status = Some(format!("Task {id} no longer exists."));
            return Action::None;
        };
        if let Some(message) = self.refusal(i, TaskChange::Remove.verb()) {
            self.status = Some(message);
            return Action::None;
        }
        self.status = None;
        Action::ChangeTask {
            id,
            change: TaskChange::Remove,
        }
    }

    /// The `e` key (T4.3): opens the shared input dialog as the edit dialog,
    /// prefilled with the selected pending task's description. The add
    /// draft is parked until the dialog closes. A refused row or a refused
    /// modal push leaves the dialog untouched.
    pub(crate) fn edit_selected_task(&mut self) -> Action {
        let Some(i) = self.changeable_task("edited") else {
            return Action::None;
        };
        let id = self.run.tasks[i].id.clone();
        let description = self.run.tasks[i].description.clone();
        if self.modals.push(Modal::Dialog) {
            self.dialog.parked = Some(std::mem::take(&mut self.dialog.editor));
            self.dialog.editor.insert(&description);
            self.dialog.kind = DialogKind::Edit;
            self.dialog.edit_id = id;
            self.dialog.status = None;
        }
        Action::None
    }

    /// The success hook: the cursor follows the changed task to the index the
    /// engine put it at. `run.tasks` is still the list from before the change
    /// here, so the new index is derived from it.
    pub(crate) fn on_task_changed(&mut self, id: &str, change: TaskChange) {
        let tasks = &self.run.tasks;
        let Some(i) = tasks.iter().position(|t| t.id == id) else {
            return;
        };
        let target = match change {
            TaskChange::Remove | TaskChange::Outdent => i,
            TaskChange::MoveUp => tasks[..i]
                .iter()
                .rposition(|t| !t.is_malformed())
                .unwrap_or(i),
            TaskChange::MoveDown => tasks[i + 1..]
                .iter()
                .position(|t| !t.is_malformed())
                .map_or(i, |p| i + 1 + p),
            TaskChange::Indent => {
                let group = tasks[i].group_number();
                tasks[..i]
                    .iter()
                    .rposition(|t| !t.is_malformed() && t.group_number() != group)
                    .map_or(i, |p| p + 1)
            }
        };
        // A removal with no explicit cursor keeps following the first pending task.
        if change == TaskChange::Remove && self.task_cursor.is_none() {
            return;
        }
        self.task_cursor = Some(target);
        self.task_scroll = 0;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use patok_core::event::{Phase, Snapshot};
    use patok_core::pipeline::PipelineState;

    use super::*;

    fn app_with(text: &str) -> App {
        App::new(
            Snapshot {
                project_dir: "/home/user/demo".into(),
                phase: Phase::Startup,
                tasks: patok_core::task::parse(text),
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
        )
    }

    const FOUR: &str = "- [ ] T1.1: a\n- [ ] T1.2: b\n- [ ] T2.1: c\n- [ ] T2.2: d\n";

    fn cursor_after(app: &mut App, from: usize, change: TaskChange) -> Option<usize> {
        app.task_cursor = Some(from);
        let id = app.run.tasks[from].id.clone();
        app.on_task_changed(&id, change);
        app.task_cursor
    }

    #[test]
    fn cursor_follows_moves() {
        let mut app = app_with(FOUR);
        assert_eq!(cursor_after(&mut app, 1, TaskChange::MoveUp), Some(0));
        assert_eq!(cursor_after(&mut app, 1, TaskChange::MoveDown), Some(2));
    }

    #[test]
    fn moves_skip_malformed_rows() {
        let mut app = app_with("- [ ] T1.1: a\n- [ ] no id here\n- [ ] T1.2: b\n");
        assert!(app.run.tasks[1].is_malformed());
        assert_eq!(cursor_after(&mut app, 0, TaskChange::MoveDown), Some(2));
        assert_eq!(cursor_after(&mut app, 2, TaskChange::MoveUp), Some(0));
    }

    #[test]
    fn indent_lands_after_the_nearest_other_group_row() {
        let mut app = app_with(FOUR);
        assert_eq!(cursor_after(&mut app, 3, TaskChange::Indent), Some(2));
        assert_eq!(cursor_after(&mut app, 2, TaskChange::Indent), Some(2));
    }

    #[test]
    fn outdent_and_remove_keep_the_index() {
        let mut app = app_with(FOUR);
        assert_eq!(cursor_after(&mut app, 2, TaskChange::Outdent), Some(2));
        assert_eq!(cursor_after(&mut app, 2, TaskChange::Remove), Some(2));
    }

    #[test]
    fn remove_without_a_cursor_keeps_following_the_first_pending_task() {
        let mut app = app_with(FOUR);
        app.on_task_changed("T1.1", TaskChange::Remove);
        assert_eq!(app.task_cursor, None);
    }

    #[test]
    fn selected_task_becomes_a_change_action() {
        let mut app = app_with(FOUR);
        app.task_cursor = Some(1);
        app.status = Some("old".into());
        assert_eq!(
            app.change_selected_task(TaskChange::Indent),
            Action::ChangeTask {
                id: "T1.2".into(),
                change: TaskChange::Indent
            }
        );
        assert_eq!(app.status, None);
    }

    #[test]
    fn done_running_malformed_and_empty_rows_are_refused() {
        let mut app = app_with("- [x] T1.1: a\n- [ ] T1.2: b\n- [ ] no id\n");
        app.task_cursor = Some(0);
        assert_eq!(app.change_selected_task(TaskChange::Remove), Action::None);
        assert_eq!(
            app.status.as_deref(),
            Some("Task T1.1 is completed and cannot be removed.")
        );
        app.task_cursor = Some(1);
        app.run.current_task = Some("T1.2".into());
        assert_eq!(app.change_selected_task(TaskChange::MoveUp), Action::None);
        assert_eq!(
            app.status.as_deref(),
            Some("Task T1.2 is in progress and cannot be moved.")
        );
        app.task_cursor = Some(2);
        assert_eq!(app.change_selected_task(TaskChange::Indent), Action::None);
        assert_eq!(
            app.status.as_deref(),
            Some("This task line has no valid id; fix it in TASKS.md first.")
        );
        let mut empty = app_with("");
        assert_eq!(empty.change_selected_task(TaskChange::Remove), Action::None);
        assert_eq!(empty.status.as_deref(), Some("No task to change."));
    }

    #[test]
    fn d_style_delete_opens_the_confirmation_not_the_action() {
        let mut app = app_with(FOUR);
        app.task_cursor = Some(1);
        assert_eq!(app.delete_selected_task(), Action::None);
        assert_eq!(app.modals.top(), Some(Modal::Delete));
        assert_eq!(app.delete.id, "T1.2");
        assert_eq!(app.delete.selected, 0);
    }

    #[test]
    fn refused_rows_open_no_confirmation() {
        let mut app = app_with("- [x] T1.1: a\n- [ ] T1.2: b\n- [ ] no id\n");
        app.task_cursor = Some(0);
        assert_eq!(app.delete_selected_task(), Action::None);
        assert_eq!(app.modals.top(), None);
        assert_eq!(
            app.status.as_deref(),
            Some("Task T1.1 is completed and cannot be removed.")
        );
        app.task_cursor = Some(1);
        app.run.current_task = Some("T1.2".into());
        assert_eq!(app.delete_selected_task(), Action::None);
        assert_eq!(app.modals.top(), None);
        assert_eq!(
            app.status.as_deref(),
            Some("Task T1.2 is in progress and cannot be removed.")
        );
        app.task_cursor = Some(2);
        assert_eq!(app.delete_selected_task(), Action::None);
        assert_eq!(app.modals.top(), None);
        assert_eq!(
            app.status.as_deref(),
            Some("This task line has no valid id; fix it in TASKS.md first.")
        );
        let mut empty = app_with("");
        assert_eq!(empty.delete_selected_task(), Action::None);
        assert_eq!(empty.modals.top(), None);
        assert_eq!(empty.status.as_deref(), Some("No task to change."));
    }

    #[test]
    fn confirm_delete_rechecks_the_task() {
        let mut app = app_with(FOUR);
        app.delete.id = "T1.2".into();
        assert_eq!(
            app.confirm_delete(),
            Action::ChangeTask {
                id: "T1.2".into(),
                change: TaskChange::Remove
            }
        );

        app.delete.id = "T1.2".into();
        app.run.current_task = Some("T1.2".into());
        assert_eq!(app.confirm_delete(), Action::None);
        assert_eq!(
            app.status.as_deref(),
            Some("Task T1.2 is in progress and cannot be removed.")
        );

        app.delete.id = "T1.2".into();
        app.run.current_task = None;
        app.run.tasks.retain(|t| t.id != "T1.2");
        assert_eq!(app.confirm_delete(), Action::None);
        assert_eq!(app.status.as_deref(), Some("Task T1.2 no longer exists."));
    }
}
