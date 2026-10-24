//! Mouse input (finding 19, the rest, in
//! docs/private/structure-review-followup.md): the main view's mouse
//! handler and the frame scrolling behind it, plus the task cursor's
//! movement (T4.1), split out of `app`. The open
//! modal stack swallows every event first; the modals' own mouse handling
//! lives in `modal`.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use patok_core::event::Phase;

use crate::app::{Action, App};
use crate::hit::{chip_clicked, frame_at};
use crate::modal::ModalLayer;
use crate::view::FrameFocus;

impl App {
    /// Scrolls one frame's content by `delta` visual lines, clamped at both ends by
    /// the frame's last-rendered range; a scroll back to 0 restores auto-follow.
    fn scroll_frame(&mut self, frame: FrameFocus, delta: isize) {
        match frame {
            FrameFocus::Output => {
                self.scroll = self
                    .scroll
                    .saturating_add_signed(delta)
                    .min(self.layout.max_scroll.get());
            }
            FrameFocus::Tasks => {
                self.task_scroll = self
                    .task_scroll
                    .saturating_add_signed(delta)
                    .min(self.layout.task_max_scroll.get());
            }
        }
    }

    /// Scrolls the focused frame's content; the unfocused frame ignores the keys.
    pub(crate) fn scroll_by(&mut self, delta: isize) -> Action {
        self.scroll_frame(self.focus, delta);
        Action::None
    }

    /// Moves the Tasks frame's selected row by `delta`, clamped to the list, and
    /// brings the view back to it (T4.1).
    pub(crate) fn move_task_cursor(&mut self, delta: isize) -> Action {
        match self.selected_task_index() {
            Some(current) => {
                let last = self.run.tasks.len() - 1;
                self.task_cursor = Some(current.saturating_add_signed(delta).min(last));
                self.task_scroll = 0;
            }
            None => self.task_cursor = None,
        }
        Action::None
    }

    /// Mouse events on the merged view: a click inside one of the two frames
    /// focuses it; a click on the status bar's interactive chips runs the
    /// action its key triggers -- the m menu chip opens the m menu (T133.1),
    /// the idle Enter chip starts the build loop or runs a discovery round,
    /// and the running Esc stop chip opens the stop dialog (T134.1) -- and a
    /// wheel step scrolls the frame the pointer is over, one line per step,
    /// without moving the focus. Events outside both frames and open modals
    /// change nothing. Every open modal handles its own mouse events and
    /// swallows the rest, so nothing underneath reacts while it is open: the
    /// theme picker keeps its row hover and click behaviour (T43.1) and every
    /// modal's bottom-line buttons respond to a click on their rectangle with
    /// exactly the action their key triggers (T59.1).
    pub fn on_mouse(&mut self, mouse: MouseEvent) -> Action {
        // Every open modal handles its own mouse events and swallows the
        // rest, so nothing underneath reacts while it is open -- the theme
        // picker keeps its row hover and click behaviour (T43.1) and every
        // modal's bottom-line buttons respond to a click on their rectangle
        // with exactly the action their key triggers (T59.1). The top of
        // the modal stack is the one modal that wins; the key dispatch and
        // the render order read the same stack (finding 20).
        if let Some(modal) = self.modals.top() {
            return modal.on_mouse(self, mouse);
        }
        let position = ratatui::layout::Position::new(mouse.column, mouse.row);
        let frame = frame_at(
            position,
            self.layout.output_area.get(),
            self.layout.tasks_area.get(),
        );
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if chip_clicked(&mouse, self.layout.menu_chip.get()) {
                    // The m key's open path, verbatim: the chip is its
                    // mouse alias (T133.1).
                    self.status = None;
                    self.open_menu();
                    Action::None
                } else if chip_clicked(&mouse, self.layout.enter_chip.get()) {
                    // The Enter chip is the Enter key's mouse alias
                    // (T134.1): start the build loop or run a discovery
                    // round, exactly the key's action -- `enter_key` carries
                    // the key's busy guard too.
                    self.enter_key()
                } else if chip_clicked(&mouse, self.layout.stop_chip.get())
                    && (self.run.phase == Phase::Running
                        || self.run.planning
                        || self.run.discovering)
                {
                    // The Esc chip is the Esc key's mouse alias (T134.1):
                    // it opens the stop dialog, the key's action while a
                    // build, planner or discovery run is active. The guard
                    // mirrors the key arm, which covers these runs too
                    // (T142.1), so the chip stays inert in any other state
                    // -- the zero rect makes those miss naturally.
                    self.status = None;
                    self.open_stop();
                    Action::None
                } else {
                    if let Some(frame) = frame {
                        self.focus = frame;
                    }
                    Action::None
                }
            }
            // Wheel steps scroll the hovered frame only; the focus stays put.
            MouseEventKind::ScrollUp => {
                if let Some(frame) = frame {
                    self.scroll_frame(frame, 1);
                }
                Action::None
            }
            MouseEventKind::ScrollDown => {
                if let Some(frame) = frame {
                    self.scroll_frame(frame, -1);
                }
                Action::None
            }
            _ => Action::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use patok_config::SettingValue;
    use patok_core::event::Snapshot;
    use patok_core::pipeline::PipelineState;
    use ratatui::layout::Rect;

    use crate::overlay::{Entry, Schema};

    fn app() -> App {
        App::new(
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
        )
    }

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// A mouse event of `kind` at (`column`, `row`).
    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// An app with the settings overlay open over a recorded 10-row list
    /// viewport (T136.1); the close and footer rects stay zero, so clicks
    /// there miss naturally.
    fn settings_app() -> (App, Rect) {
        // No provider readout here: model rows keep the inline editor (T4.4).
        let mut app = app();
        app.open_settings();
        let list = Rect::new(10, 5, 60, 10);
        app.overlay.list.set(list);
        (app, list)
    }

    /// A dirty settings overlay with its unsaved-changes dialog open (T146.1):
    /// the app, the recorded list viewport and the recorded choice-rows area.
    /// The body rect lies deliberately outside the list rect so list
    /// hit-testing never interferes; its rows 0-2 are the three choices and
    /// rows 3-5 the blank padding below them.
    fn confirm_app() -> (App, Rect, Rect) {
        let (mut app, list) = settings_app();
        app.overlay
            .drafts
            .insert("builder_model".into(), SettingValue::Str("opus".into()));
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(app.confirm_open());
        assert_eq!(app.overlay.confirm_selected, 0);
        let body = Rect::new(0, 20, 50, 6);
        app.overlay.confirm_body.set(body);
        (app, list, body)
    }

    /// A left click on the list's `index`-th visible row.
    fn click_row(app: &mut App, list: Rect, index: usize) -> Action {
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            list.x + 2,
            list.y + index as u16,
        ))
    }

    /// The visible index of `field`'s row.
    fn visible_index(app: &App, field: &str) -> usize {
        app.overlay
            .visible()
            .iter()
            .position(|entry| matches!(entry, Entry::Row(row) if row.key == field))
            .unwrap_or_else(|| panic!("`{field}` is not visible"))
    }

    /// Wheel steps scroll the list one row per step without moving the
    /// selection, clamped at the first and the last row (T136.1).
    #[test]
    fn wheel_steps_scroll_the_list_without_moving_the_selection() {
        let (mut app, list) = settings_app();
        let len = app.overlay.visible().len();
        let over = (list.x + 2, list.y + 2);

        // One wheel step moves the offset exactly one row; the selection,
        // the drafts and the editor stay put.
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::ScrollDown, over.0, over.1)),
            Action::None
        );
        assert_eq!(app.overlay.scroll.get(), 1);
        assert_eq!(app.overlay.focus, 0);
        assert!(app.overlay.drafts.is_empty());
        assert!(app.overlay.editor.is_none());

        // Scrolling down past the end clamps at the last offset that still
        // fills the viewport.
        for _ in 0..len {
            app.on_mouse(mouse(MouseEventKind::ScrollDown, over.0, over.1));
        }
        assert_eq!(app.overlay.scroll.get(), len - 10);
        assert_eq!(app.overlay.focus, 0);

        // Scrolling back up clamps at the first row.
        for _ in 0..(len + 10) {
            app.on_mouse(mouse(MouseEventKind::ScrollUp, over.0, over.1));
        }
        assert_eq!(app.overlay.scroll.get(), 0);
        assert_eq!(app.overlay.focus, 0);
        assert!(app.overlay.drafts.is_empty());
        assert!(app.overlay.editor.is_none());
    }

    /// A left click inside a list row selects it and runs its Enter key
    /// verbatim: a header folds, a Text row opens the inline editor and a
    /// Bool row drafts the toggle (T136.1).
    #[test]
    fn a_click_on_a_row_selects_and_runs_its_enter() {
        let (mut app, list) = settings_app();
        let full = app.overlay.visible().len();

        // A header click folds its section, and a second one unfolds it --
        // exactly Enter's behaviour on a header.
        assert_eq!(click_row(&mut app, list, 0), Action::None);
        assert_eq!(app.overlay.focus, 0);
        assert!(app.overlay.visible().len() < full);
        assert_eq!(click_row(&mut app, list, 0), Action::None);
        assert_eq!(app.overlay.visible().len(), full);

        // A Text row's click opens the same inline editor Enter opens,
        // prefilled with the row's current value (no readout yet: empty).
        let model = visible_index(&app, "builder_model");
        assert_eq!(click_row(&mut app, list, model), Action::None);
        assert_eq!(app.overlay.focus, model);
        let editor = app
            .overlay
            .editor
            .take()
            .expect("the click opened the editor");
        assert_eq!(editor.field, "builder_model");
        assert_eq!(editor.buffer, "");

        // A Bool row's click drafts the toggle Enter drafts. The wheel
        // scrolled it into view first, so the click also proves the
        // hit-test sees the scrolled rows.
        for _ in 0..12 {
            app.on_mouse(mouse(MouseEventKind::ScrollDown, list.x + 2, list.y + 2));
        }
        let plan = visible_index(&app, "plan_enabled");
        let scrolled = plan - app.overlay.scroll.get();
        assert_eq!(click_row(&mut app, list, scrolled), Action::None);
        assert_eq!(app.overlay.focus, plan);
        assert_eq!(
            app.overlay.drafts.get("plan_enabled"),
            Some(&SettingValue::Bool(true))
        );
    }

    /// A click on a blank viewport row below the list's last entry and a
    /// click outside the list rect entirely change nothing (T136.1).
    #[test]
    fn a_click_outside_the_list_rows_does_nothing() {
        let (mut app, list) = settings_app();
        // Fold every section by clicking its header: the list holds only
        // its headers, leaving blank viewport rows below them.
        for index in 0..crate::overlay::SECTIONS.len() {
            let header = app
                .overlay
                .visible()
                .iter()
                .position(|entry| *entry == Entry::Header(index))
                .unwrap();
            assert_eq!(click_row(&mut app, list, header), Action::None);
        }
        assert_eq!(app.overlay.visible().len(), crate::overlay::SECTIONS.len());

        let before = (app.overlay.focus, app.overlay.scroll.get());
        assert_eq!(click_row(&mut app, list, 7), Action::None, "a blank row");
        // (0, 1) is outside the list rect and below the footer line the zero
        // rects still derive their phantom buttons on.
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 1)),
            Action::None,
            "outside the list rect"
        );
        assert_eq!((app.overlay.focus, app.overlay.scroll.get()), before);
        assert!(app.overlay.drafts.is_empty());
        assert!(app.overlay.editor.is_none());
    }

    /// The inline editor is modal for the mouse too: wheel steps over the
    /// overlay are swallowed while it is open (T136.1).
    #[test]
    fn wheel_steps_are_swallowed_while_the_editor_is_open() {
        let (mut app, list) = settings_app();
        // The same Enter path a click on a Text row runs opens the editor.
        let model = visible_index(&app, "builder_model");
        click_row(&mut app, list, model);
        assert!(app.overlay.editor.is_some());
        for _ in 0..5 {
            app.on_mouse(mouse(MouseEventKind::ScrollDown, list.x + 2, list.y + 2));
        }
        assert_eq!(app.overlay.scroll.get(), 0, "the editor swallows the wheel");
    }

    /// A hidden model row (T3.2) leaves the hit-test: the row after the provider
    /// row is the next row that is shown.
    #[test]
    fn hovering_rows_skips_a_hidden_model_row() {
        let mut app = app();
        app.settings
            .insert("builder_provider".into(), SettingValue::Str("vibe".into()));
        app.open_settings();
        let list = Rect::new(10, 5, 60, 10);
        app.overlay.list.set(list);
        let provider = visible_index(&app, "builder_provider");
        assert_eq!(visible_index(&app, "reviewer_provider"), provider + 1);
        app.on_mouse(mouse(
            MouseEventKind::Moved,
            list.x + 2,
            list.y + provider as u16 + 1,
        ));
        assert_eq!(app.overlay.focused_row().unwrap().key, "reviewer_provider");
    }

    /// A pointer move inside a visible list row moves the selection highlight
    /// to that row without running it -- no editor, no drafts, no folds --
    /// and Enter on a hovered row opens the same inline editor the click and
    /// the keyboard paths open (T145.1).
    #[test]
    fn hovering_a_row_moves_the_selection_without_running_it() {
        let (mut app, list) = settings_app();
        let len = app.overlay.visible().len();
        for row in 0..10 {
            // The pointer rides viewport row `row`; the hit-test resolves it
            // against the current offset, so the highlight lands on the row
            // under the pointer and the follow keeps that row on screen.
            let hovered = app.overlay.scroll.get() + row;
            assert_eq!(
                app.on_mouse(mouse(
                    MouseEventKind::Moved,
                    list.x + 2,
                    list.y + row as u16
                )),
                Action::None
            );
            assert_eq!(app.overlay.focus, hovered);
            assert!(app.overlay.focus >= app.overlay.scroll.get());
            assert!(app.overlay.focus - app.overlay.scroll.get() < 10);
            assert!(app.settings_open());
            assert!(app.overlay.editor.is_none());
            assert!(app.overlay.drafts.is_empty());
            assert!(!app.confirm_open());
            assert_eq!(
                app.overlay.visible().len(),
                len,
                "a hover never folds a header"
            );
        }

        // Enter on a hovered row runs exactly the keyboard path: the same
        // inline editor the click opens, prefilled with the row's current
        // value (no readout yet: empty).
        let (mut app, list) = settings_app();
        let model = visible_index(&app, "builder_model");
        assert_eq!(
            app.on_mouse(mouse(
                MouseEventKind::Moved,
                list.x + 2,
                list.y + model as u16
            )),
            Action::None
        );
        assert_eq!(app.overlay.focus, model);
        assert!(
            app.overlay.editor.is_none(),
            "the hover never activates the row"
        );
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        let editor = app
            .overlay
            .editor
            .take()
            .expect("Enter on the hovered row opens the editor");
        assert_eq!(editor.field, "builder_model");
        assert_eq!(editor.buffer, "");

        // The click path is untouched by the hover (T136.1): a click on a
        // different row still selects and activates the clicked row, not
        // the hovered one.
        let (mut app, list) = settings_app();
        let provider = visible_index(&app, "builder_provider");
        let model = visible_index(&app, "builder_model");
        assert_eq!(
            app.on_mouse(mouse(
                MouseEventKind::Moved,
                list.x + 2,
                list.y + provider as u16
            )),
            Action::None
        );
        assert_eq!(app.overlay.focus, provider);
        assert!(
            app.overlay.editor.is_none(),
            "the hover never activates the row"
        );
        let scrolled_model = model.saturating_sub(app.overlay.scroll.get());
        assert_eq!(click_row(&mut app, list, scrolled_model), Action::None);
        assert_eq!(app.overlay.focus, model);
        let editor = app
            .overlay
            .editor
            .take()
            .expect("the click still opens the editor");
        assert_eq!(editor.field, "builder_model");
        assert_eq!(editor.buffer, "");
    }

    /// A pointer move over the blank viewport rows past the last entry, over
    /// the modal above the list rect, or outside the overlay entirely leaves
    /// the selection and the offset alone (T145.1).
    #[test]
    fn hovering_between_the_rows_or_outside_the_list_changes_nothing() {
        let (mut app, list) = settings_app();
        // Fold every section by clicking its header, exactly the T136.1
        // path: the list holds only its headers, leaving blank
        // viewport rows below them.
        for index in 0..crate::overlay::SECTIONS.len() {
            let header = app
                .overlay
                .visible()
                .iter()
                .position(|entry| *entry == Entry::Header(index))
                .unwrap();
            assert_eq!(click_row(&mut app, list, header), Action::None);
        }
        assert_eq!(app.overlay.visible().len(), crate::overlay::SECTIONS.len());

        // An observable focus the misses must leave alone.
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Moved, list.x + 2, list.y + 1)),
            Action::None
        );
        assert_eq!(app.overlay.focus, 1);
        let before = (app.overlay.focus, app.overlay.scroll.get());

        // A blank viewport row below the last header, the border row above
        // the recorded list rect, and a point outside the overlay entirely.
        for (column, row) in [(list.x + 2, list.y + 7), (list.x + 2, list.y - 1), (0, 1)] {
            assert_eq!(
                app.on_mouse(mouse(MouseEventKind::Moved, column, row)),
                Action::None
            );
            assert_eq!((app.overlay.focus, app.overlay.scroll.get()), before);
        }
        assert!(app.overlay.drafts.is_empty());
        assert!(app.overlay.editor.is_none());
    }

    /// Hovering the viewport's edge rows scrolls the list exactly the way
    /// the navigation keys do: the offset follows with the margin band and
    /// clamps at the first and the last row, and the walk never activates
    /// anything (T145.1).
    #[test]
    fn hovering_the_edges_of_a_long_list_scrolls_it_like_the_keys() {
        let (mut app, list) = settings_app();
        let len = app.overlay.visible().len();

        // Hovering the bottom viewport row from the top of the list lands
        // exactly where nine Down presses land: focus 9 with the offset 4.
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Moved, list.x + 2, list.y + 9)),
            Action::None
        );
        assert_eq!(app.overlay.focus, 9);
        assert_eq!(app.overlay.scroll.get(), 4);

        // Keeping the pointer on the bottom row walks the list down: each
        // move lands on the row the follow brought under the pointer, the
        // viewport keeps the hovered row visible, and the walk ends on the
        // last row at the last offset that still fills the viewport.
        while app.overlay.focus < len - 1 {
            assert!(app.overlay.editor.is_none());
            assert!(app.overlay.drafts.is_empty());
            let hovered = app.overlay.scroll.get() + 9;
            assert_eq!(
                app.on_mouse(mouse(MouseEventKind::Moved, list.x + 2, list.y + 9)),
                Action::None
            );
            assert_eq!(app.overlay.focus, hovered);
            assert!(app.overlay.focus >= app.overlay.scroll.get());
            assert!(app.overlay.focus - app.overlay.scroll.get() < 10);
        }
        assert_eq!(app.overlay.focus, len - 1);
        assert_eq!(
            app.overlay.scroll.get(),
            len - 10,
            "never overshooting the clamp at the last full screen"
        );

        // Walking back up on the top viewport row ends at the first row
        // with the offset clamped at 0.
        while app.overlay.focus > 0 {
            let hovered = app.overlay.scroll.get();
            assert_eq!(
                app.on_mouse(mouse(MouseEventKind::Moved, list.x + 2, list.y)),
                Action::None
            );
            assert_eq!(app.overlay.focus, hovered);
            assert!(app.overlay.focus >= app.overlay.scroll.get());
            assert!(app.overlay.focus - app.overlay.scroll.get() < 10);
        }
        assert_eq!(app.overlay.focus, 0);
        assert_eq!(
            app.overlay.scroll.get(),
            0,
            "never overshooting the clamp at the first row"
        );
    }

    /// The inline editor is modal for the pointer moves too: a hover is
    /// swallowed while it is open.
    #[test]
    fn hovering_is_swallowed_while_the_editor_is_open() {
        let (mut app, list) = settings_app();
        let model = visible_index(&app, "builder_model");
        click_row(&mut app, list, model);
        assert!(app.overlay.editor.is_some());
        for row in 0..10 {
            assert_eq!(
                app.on_mouse(mouse(
                    MouseEventKind::Moved,
                    list.x + 2,
                    list.y + row as u16
                )),
                Action::None
            );
            assert_eq!(app.overlay.focus, model);
            assert!(app.overlay.editor.is_some());
        }

        // The draft stays drafted; the dialog's own hover behaviour is
        // covered by the T146.1 tests below.
        for c in "opus".chars() {
            assert_eq!(press(&mut app, KeyCode::Char(c)), Action::None);
        }
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert!(app.overlay.editor.is_none());
        assert_eq!(
            app.overlay.drafts.get("builder_model"),
            Some(&SettingValue::Str("opus".into()))
        );
    }

    /// The unsaved-changes dialog follows the pointer (T146.1): a move onto
    /// one of the three choice rows moves the selection highlight there
    /// without confirming it, and a move inside the dialog that misses every
    /// row -- the blank padding below the choices -- or over the list rect
    /// behind it, or outside the dialog, leaves the selection alone.
    #[test]
    fn hovering_the_confirm_dialog_follows_the_pointer() {
        let (mut app, list, body) = confirm_app();
        for row in 0..3 {
            assert_eq!(
                app.on_mouse(mouse(
                    MouseEventKind::Moved,
                    body.x + 2,
                    body.y + row as u16
                )),
                Action::None
            );
            assert_eq!(app.overlay.confirm_selected, row);
            assert!(app.confirm_open());
            assert_eq!(
                app.overlay.drafts.get("builder_model"),
                Some(&SettingValue::Str("opus".into()))
            );
        }

        // The misses leave the selection on the last hovered row: the blank
        // body padding below the choices, the list rect behind the dialog and
        // the shell outside.
        for (column, row) in [
            (body.x + 2, body.y + 3),
            (body.x + 2, body.y + 5),
            (body.x + body.width, body.y),
            (list.x + 2, list.y + 3),
            (0, 0),
        ] {
            assert_eq!(
                app.on_mouse(mouse(MouseEventKind::Moved, column, row)),
                Action::None
            );
            assert_eq!(app.overlay.confirm_selected, 2);
            assert!(app.confirm_open());
        }
    }

    /// A hover picks the row the Enter key then confirms (T146.1): the dialog
    /// runs its Enter dispatch on the hovered row, exactly the key path.
    #[test]
    fn enter_after_a_hover_confirms_the_hovered_choice() {
        // Hovering Discard (row 1) and pressing Enter discards and closes.
        let (mut app, _list, body) = confirm_app();
        app.on_mouse(mouse(MouseEventKind::Moved, body.x + 2, body.y + 1));
        assert_eq!(app.overlay.confirm_selected, 1);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::CloseSettings);
        assert!(!app.confirm_open());
        assert!(!app.settings_open());
        assert!(app.overlay.drafts.is_empty());

        // Hovering Cancel (row 2) and pressing Enter returns to the overlay
        // with the dialog closed and the drafts intact.
        let (mut app, _list, body) = confirm_app();
        app.on_mouse(mouse(MouseEventKind::Moved, body.x + 2, body.y + 2));
        assert_eq!(app.overlay.confirm_selected, 2);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::None);
        assert!(!app.confirm_open());
        assert!(app.settings_open());
        assert_eq!(
            app.overlay.drafts.get("builder_model"),
            Some(&SettingValue::Str("opus".into()))
        );
    }

    /// A left click inside a choice row's rect runs exactly that row's Enter
    /// key (T146.1): Save asks the driver to apply the drafts, Discard throws
    /// them away and closes, Cancel returns to the overlay -- and a click in
    /// the padding between the rows or outside the dialog does nothing.
    #[test]
    fn a_click_on_a_confirm_choice_row_runs_its_enter() {
        // Save (row 0): the overlay stays open for the driver's
        // apply-and-persist flow, with the drafts still pending.
        let (mut app, _list, body) = confirm_app();
        assert_eq!(
            app.on_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                body.x + 2,
                body.y
            )),
            Action::SaveSettings
        );
        assert!(!app.confirm_open());
        assert!(app.settings_open());
        assert_eq!(
            app.overlay.drafts.get("builder_model"),
            Some(&SettingValue::Str("opus".into()))
        );
        assert_eq!(
            app.overlay.pending(),
            vec![(
                Schema::Daemon,
                "builder_model".to_string(),
                SettingValue::Str("opus".into())
            )]
        );

        // Discard (row 1): the drafts are thrown away and the overlay closes.
        let (mut app, _list, body) = confirm_app();
        assert_eq!(
            app.on_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                body.x + 2,
                body.y + 1
            )),
            Action::CloseSettings
        );
        assert!(!app.confirm_open());
        assert!(!app.settings_open());
        assert!(app.overlay.drafts.is_empty());

        // Cancel (row 2): only the dialog closes; the overlay and the drafts
        // stay as they were.
        let (mut app, _list, body) = confirm_app();
        assert_eq!(
            app.on_mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                body.x + 2,
                body.y + 2
            )),
            Action::None
        );
        assert!(!app.confirm_open());
        assert!(app.settings_open());
        assert_eq!(
            app.overlay.drafts.get("builder_model"),
            Some(&SettingValue::Str("opus".into()))
        );

        // The misses do nothing: the padding below the choices and the shell
        // outside the dialog leave the dialog open with the drafts intact.
        // The outside point sits off row 0, where the zero footer rect's
        // phantom button rects would otherwise catch the click.
        let (mut app, _list, body) = confirm_app();
        for (column, row) in [(body.x + 2, body.y + 3), (body.x + 2, body.y + 5), (0, 10)] {
            assert_eq!(
                app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), column, row)),
                Action::None
            );
            assert!(app.confirm_open());
            assert_eq!(app.overlay.confirm_selected, 0);
            assert_eq!(
                app.overlay.drafts.get("builder_model"),
                Some(&SettingValue::Str("opus".into()))
            );
        }
    }

    /// The dialog's keys are unchanged (T146.1): j, k and the arrows move the
    /// selection while it is open, Esc closes only the dialog with the drafts
    /// intact -- and with the dialog closed, a move over the recorded choice
    /// rows' coordinates goes through the overlay's mouse path, misses the
    /// list rect and touches neither the list selection nor the dialog's.
    #[test]
    fn the_confirm_dialog_keys_stay_unchanged() {
        let (mut app, _list, body) = confirm_app();
        assert_eq!(app.overlay.confirm_selected, 0);
        assert_eq!(press(&mut app, KeyCode::Char('k')), Action::None);
        assert_eq!(app.overlay.confirm_selected, 0);
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
        assert_eq!(app.overlay.confirm_selected, 1);
        assert_eq!(press(&mut app, KeyCode::Char('j')), Action::None);
        assert_eq!(app.overlay.confirm_selected, 2);
        assert_eq!(press(&mut app, KeyCode::Up), Action::None);
        assert_eq!(app.overlay.confirm_selected, 1);
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert!(!app.confirm_open());
        assert!(app.settings_open());
        assert_eq!(
            app.overlay.drafts.get("builder_model"),
            Some(&SettingValue::Str("opus".into()))
        );

        // With the dialog closed, the same coordinates go through the
        // overlay's list hover and miss: the recorded choice-rows area lies
        // outside the list rect, so the focus and the dialog's selection
        // stay put.
        let focus = app.overlay.focus;
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Moved, body.x + 2, body.y + 1)),
            Action::None
        );
        assert_eq!(app.overlay.focus, focus);
        assert_eq!(app.overlay.confirm_selected, 1);
    }
}
