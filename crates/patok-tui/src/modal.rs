//! The modal stack and the one interface every modal implements (finding 20,
//! the rest, in docs/private/structure-review-followup.md): which modal is
//! open is one stack on [`crate::app::App`] instead of seven independent open
//! flags, and the area, the footer buttons, the key handling, the mouse
//! handling and the render of each modal live behind the [`ModalLayer`]
//! trait once, instead of seven `on_*_key` / `on_*_mouse` pairs on `App`.

use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use patok_core::event::Phase;
use ratatui::Frame;
use ratatui::layout::Rect;

use crate::app::{Action, App, MenuChoice, SOFT_STOP_PENDING, menu_entries};
use crate::choice::{ChoiceKey, ChoiceMouse, choice_area, choice_key, choice_mouse};
use crate::hit::{
    chip_clicked, close_clicked, footer_button_code, menu_row_at, settings_row_at, theme_row_at,
};
use crate::overlay::{Entry, SETTINGS_CHOICES, header_fold};
use crate::ui::{dialogs, settings};
use crate::view::{STOP_CHOICES, StopChoice};

/// Which modal is open (finding 20): one enum over the modals plus the
/// settings overlay's nested unsaved-changes dialog. The modal stack
/// ([`ModalStack`]) is the writable state: which of these are open, and in
/// what stacking order. This enum is only their identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modal {
    /// The settings overlay.
    Overlay,
    /// The add-task / inject-task dialog.
    Dialog,
    /// The m menu.
    Menu,
    /// The stop dialog.
    Stop,
    /// The settings overlay's unsaved-changes dialog, stacked on it.
    Confirm,
    /// The theme picker.
    Theme,
    /// The interactive provider picker.
    InteractivePicker,
    /// The embedded provider terminal (T2.3).
    Terminal,
}

/// The open modals, one stack (finding 20, the rest): the bottom modal first,
/// the top modal -- the one that wins keys, mouse and the top pixel layer --
/// last. Key dispatch, mouse dispatch and the render order all read this one
/// stack, so they cannot disagree.
///
/// Which combinations are legitimately open together, from reading every
/// opener: exactly one. The unsaved-changes confirm dialog over a dirty
/// settings overlay -- [`Modal::Overlay`] with [`Modal::Confirm`] above it --
/// created solely by the overlay's own close path (Esc or q on a dirty
/// overlay), popped by its Cancel choice and closed together with the
/// overlay by its Save and Discard choices. Every other modal is opened only
/// with an empty stack: an open modal swallows every key and mouse event, so
/// the direct keys (`t`, `m`, `?`, `a`, `i` and Esc while a run is active)
/// and the three status-bar chips never run while another modal is open.
/// The m menu's Enter entries are replacements, not stacks -- the menu
/// closes first, then the target opens -- and a click on the menu chip while
/// the menu is open closes it. So [`ModalStack::push`] accepts only an empty
/// stack or the confirm over the overlay; every other push is a refused
/// no-op.
#[derive(Default)]
pub(crate) struct ModalStack {
    /// The open modals, bottom first.
    modals: Vec<Modal>,
}

impl ModalStack {
    /// The empty stack: no modal open.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Stacks `modal` on the top, or refuses it as a no-op: see the struct's
    /// documentation for the one legitimate combination. Returns whether the
    /// modal was stacked, so a refused open can leave every other piece of
    /// state untouched.
    pub(crate) fn push(&mut self, modal: Modal) -> bool {
        let stacks = match (self.modals.last(), modal) {
            // A modal opens on the empty stack; the confirm is not a modal of
            // its own, it exists only stacked on the overlay.
            (None, Modal::Confirm) => false,
            (None, _) => true,
            // The one legitimate co-open pair: the unsaved-changes dialog
            // over the dirty settings overlay.
            (Some(&Modal::Overlay), Modal::Confirm) => true,
            (Some(_), _) => false,
        };
        if stacks {
            self.modals.push(modal);
        }
        stacks
    }

    /// Removes `modal` and every modal stacked above it, so closing the
    /// settings overlay also dismisses the unsaved-changes dialog nested in
    /// it. A modal that is not on the stack is a no-op.
    pub(crate) fn pop(&mut self, modal: Modal) {
        if let Some(at) = self.modals.iter().position(|open| *open == modal) {
            self.modals.truncate(at);
        }
    }

    /// The top modal: the one that wins keys, mouse and the top pixel layer,
    /// `None` when no modal is open. Pure, so callers can then take `&mut`.
    pub(crate) fn top(&self) -> Option<Modal> {
        self.modals.last().copied()
    }

    /// Whether `modal` is anywhere on the stack.
    pub(crate) fn contains(&self, modal: Modal) -> bool {
        self.modals.contains(&modal)
    }

    /// Whether no modal is open.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.modals.is_empty()
    }

    /// The open modals, bottom first: the render order, the bottom layer
    /// drawn first and the top modal last.
    pub(crate) fn iter(&self) -> impl Iterator<Item = Modal> + '_ {
        self.modals.iter().copied()
    }
}

/// The m menu's footer buttons (finding 20): defined once beside the menu's
/// rows, shared by the renderer and the mouse hit-test, so the drawn buttons
/// and their hit-test rectangles agree by construction.
pub(crate) const MENU_BUTTONS: [(&str, &str); 2] = [("Enter", "Confirm"), ("Esc", "Close")];

/// The interactive picker's footer buttons: shared by the renderer and the
/// mouse hit-test.
pub(crate) const PICKER_BUTTONS: [(&str, &str); 2] = [("Enter", "Open"), ("Esc", "Close")];

/// The embedded terminal's footer button: the exit chord, shared by the
/// renderer and the mouse hit-test.
pub(crate) const TERMINAL_BUTTONS: [(&str, &str); 1] = [("Ctrl+]", "Close")];

/// The theme picker's footer buttons (finding 20): shared by the renderer
/// and the mouse hit-test.
pub(crate) const THEME_BUTTONS: [(&str, &str); 2] = [("Enter", "Save"), ("Esc", "Cancel")];

/// The settings overlay's footer buttons (finding 20): shared by the
/// renderer and the mouse hit-test.
pub(crate) const OVERLAY_BUTTONS: [(&str, &str); 1] = [("Esc", "Close")];

/// The stop dialog's footer Esc label (finding 20): the one spelling its
/// renderer and its mouse hit-test share.
pub(crate) const STOP_ESC_LABEL: &str = "Close";

/// The stop dialog's footer buttons (finding 20): the shared choice-dialog
/// footer pairs built from the stop dialog's Esc label.
pub(crate) const STOP_BUTTONS: [(&str, &str); 2] = [("Enter", "Confirm"), ("Esc", STOP_ESC_LABEL)];

/// The unsaved-changes dialog's footer Esc label (finding 20): the one
/// spelling its renderer and its mouse hit-test share.
pub(crate) const CONFIRM_ESC_LABEL: &str = "Cancel";

/// The unsaved-changes dialog's footer buttons (finding 20): the shared
/// choice-dialog footer pairs built from the dialog's Esc label.
pub(crate) const CONFIRM_BUTTONS: [(&str, &str); 2] =
    [("Enter", "Confirm"), ("Esc", CONFIRM_ESC_LABEL)];

/// One modal's whole interface (finding 20, the rest): the area, the footer
/// buttons, the key handling, the mouse handling and the render behind one
/// trait, so each modal is defined once and the dispatch, the hit-tests and
/// the renderers all read the same definitions.
pub(crate) trait ModalLayer: Copy {
    /// The modal's rect on `screen`. The menu's height follows whether a
    /// build runs, so it reads the app.
    fn area(self, app: &App, screen: Rect) -> Rect;
    /// The modal footer's buttons: the pairs the renderer draws and the
    /// mouse hit-test reads, agreeing by construction. The dialog's primary
    /// label follows the scenario ([`App::primary_label`]), so it is the one
    /// modal whose buttons are computed rather than a table entry.
    fn buttons(self, app: &App) -> Cow<'static, [(&'static str, &'static str)]>;
    /// One key aimed at the modal; every other key is swallowed.
    fn on_key(self, app: &mut App, key: KeyEvent, ctrl: bool) -> Action;
    /// One mouse event aimed at the modal; every other event is swallowed.
    fn on_mouse(self, app: &mut App, mouse: MouseEvent) -> Action;
    /// Draws the modal on top of whatever is already in the frame.
    fn render(self, frame: &mut Frame, app: &App);
}

impl ModalLayer for Modal {
    fn area(self, app: &App, screen: Rect) -> Rect {
        match self {
            Modal::Overlay => settings::settings_area(screen),
            Modal::Confirm | Modal::Stop => choice_area(screen),
            Modal::Dialog => dialogs::dialog_area(screen),
            Modal::Menu => dialogs::menu_area(screen, app.run.phase == Phase::Running),
            Modal::Theme => dialogs::theme_area(screen),
            Modal::InteractivePicker => {
                dialogs::interactive_picker_area(screen, app.run.phase == Phase::Running)
            }
            Modal::Terminal => dialogs::terminal_area(screen),
        }
    }

    fn buttons(self, app: &App) -> Cow<'static, [(&'static str, &'static str)]> {
        match self {
            Modal::Overlay => Cow::Borrowed(&OVERLAY_BUTTONS),
            Modal::Dialog => Cow::Owned(vec![("Enter", app.primary_label()), ("Esc", "Close")]),
            Modal::Menu => Cow::Borrowed(&MENU_BUTTONS),
            Modal::Stop => Cow::Borrowed(&STOP_BUTTONS),
            Modal::Confirm => Cow::Borrowed(&CONFIRM_BUTTONS),
            Modal::Theme => Cow::Borrowed(&THEME_BUTTONS),
            Modal::InteractivePicker => Cow::Borrowed(&PICKER_BUTTONS),
            Modal::Terminal => Cow::Borrowed(&TERMINAL_BUTTONS),
        }
    }

    fn on_key(self, app: &mut App, key: KeyEvent, ctrl: bool) -> Action {
        match self {
            Modal::Overlay => overlay_key(app, key),
            Modal::Dialog => dialog_key(app, key, ctrl),
            Modal::Menu => menu_key(app, key),
            Modal::Stop => stop_key(app, key),
            // The unsaved-changes dialog's keys are the overlay's nested
            // dialog's, not the overlay's own rows'.
            Modal::Confirm => app.overlay.on_confirm_key(&mut app.modals, key.code),
            Modal::Theme => theme_key(app, key, ctrl),
            Modal::InteractivePicker => picker_key(app, key),
            Modal::Terminal => app.terminal_key(key),
        }
    }

    fn on_mouse(self, app: &mut App, mouse: MouseEvent) -> Action {
        match self {
            Modal::Overlay => overlay_mouse(app, mouse),
            Modal::Dialog => dialog_mouse(app, mouse),
            Modal::Menu => menu_mouse(app, mouse),
            Modal::Stop => stop_mouse(app, mouse),
            Modal::Confirm => confirm_mouse(app, mouse),
            Modal::Theme => theme_mouse(app, mouse),
            Modal::InteractivePicker => picker_mouse(app, mouse),
            Modal::Terminal => app.terminal_mouse(mouse),
        }
    }

    fn render(self, frame: &mut Frame, app: &App) {
        match self {
            Modal::Overlay => settings::render_settings_overlay(frame, app),
            Modal::Confirm => settings::render_settings_confirm(frame, app),
            Modal::Dialog => dialogs::render_dialog(frame, app),
            Modal::Menu => dialogs::render_menu(frame, app),
            Modal::Stop => dialogs::render_stop_dialog(frame, app),
            Modal::Theme => dialogs::render_theme_modal(frame, app),
            Modal::InteractivePicker => dialogs::render_interactive_picker(frame, app),
            Modal::Terminal => dialogs::render_terminal(frame, app),
        }
    }
}

/// A bare key press of `code`, the form the mouse handlers forward to their
/// key paths with.
fn bare(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// The settings overlay's key path: the focused row's current value is read
/// before the state is borrowed mutably.
fn overlay_key(app: &mut App, key: KeyEvent) -> Action {
    let entry = app.overlay.focused();
    let current = app.setting_value(entry);
    app.overlay.on_key(&mut app.modals, key, current)
}

/// The settings overlay's mouse handling (T59.1, T136.1): a click on the
/// bottom line's Close button runs exactly the Esc key through the
/// overlay's key path -- a clean overlay closes, a dirty one opens the
/// unsaved-changes dialog. A click on the title row's close button runs the
/// same Esc key (T66.1). A wheel step scrolls the list one row per
/// step without moving the selection, clamped at the list's ends, and a
/// left click inside a rendered list row runs that row's Enter key
/// verbatim -- it selects the row first, so a click and Enter on the
/// same row are indistinguishable. A pointer move inside a rendered
/// list row moves the selection highlight to that row with the same
/// viewport follow the Up and Down keys run, without running it
/// (T145.1). The inline editor is modal like its keys: while it is
/// open, row clicks, wheel steps and pointer moves are swallowed.
/// Everything else is swallowed, as before.
fn overlay_mouse(app: &mut App, mouse: MouseEvent) -> Action {
    if close_clicked(&mouse, app.overlay.close.get()) {
        return overlay_key(app, bare(KeyCode::Esc));
    }
    let buttons = Modal::Overlay.buttons(app);
    if let Some(code) = footer_button_code(&mouse, app.overlay.footer.get(), &buttons) {
        return overlay_key(app, bare(code));
    }
    // The inline editor is modal: while it is open, row clicks, wheel
    // steps and pointer moves are swallowed, like every key that is
    // not the editor's.
    if app.overlay.editor.is_some() {
        return Action::None;
    }
    let position = ratatui::layout::Position::new(mouse.column, mouse.row);
    let row = settings_row_at(
        position,
        app.overlay.list.get(),
        app.overlay.scroll.get(),
        app.overlay.visible().len(),
    );
    match mouse.kind {
        // The pointer move moves only the selection highlight and the
        // viewport's follow (T145.1): it never runs the row's Enter
        // key, and a move that misses every row -- the blank viewport
        // rows past the last entry, the help box, the border, the
        // footer, the shell outside -- leaves the focus and the
        // offset alone.
        MouseEventKind::Moved => {
            if let Some(index) = row {
                app.overlay.hover_focus(index);
            }
        }
        MouseEventKind::ScrollUp => {
            app.overlay.scroll_by(-1);
        }
        MouseEventKind::ScrollDown => {
            app.overlay.scroll_by(1);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(index) = row {
                // The click is the row's Enter key verbatim: it moves
                // the focus to the row, then runs the Enter dispatch,
                // which reads the row's current value -- so a click and
                // Enter on the same row are indistinguishable.
                app.overlay.focus = index;
                return overlay_key(app, bare(KeyCode::Enter));
            }
        }
        _ => {}
    }
    Action::None
}

/// The unsaved-changes dialog's mouse handling (T59.1): a click on one of
/// the bottom line's buttons runs exactly that button's key through the
/// dialog's key path -- Enter confirms the selected choice (save, discard
/// or cancel) and Esc returns to the overlay with the drafts intact. A
/// click on the title row's close button returns to the overlay the same
/// way (T66.1). A pointer move over one of the dialog's three choice rows
/// moves the selection highlight there without confirming it, and a left
/// click inside a choice row runs that row's Enter key verbatim, so a click
/// and Enter on a row are indistinguishable (T146.1). A move or click
/// inside the dialog that misses every row -- the blank body padding
/// below the choices, the border, the title -- or lands outside it does
/// nothing. Everything else is swallowed, as before.
fn confirm_mouse(app: &mut App, mouse: MouseEvent) -> Action {
    let buttons = Modal::Confirm.buttons(app);
    match choice_mouse(
        &mouse,
        app.overlay.confirm_close.get(),
        app.overlay.confirm_footer.get(),
        &buttons,
        app.overlay.confirm_body.get(),
        SETTINGS_CHOICES.len(),
    ) {
        ChoiceMouse::Key(code) => app.overlay.on_confirm_key(&mut app.modals, code),
        // The move moves only the highlight, never the dispatch.
        ChoiceMouse::Hover(index) => {
            app.overlay.confirm_selected = index;
            Action::None
        }
        // The click is the row's Enter key verbatim: it moves the
        // selection to the row, then runs the Enter dispatch, so a
        // click and Enter on the same row are indistinguishable.
        ChoiceMouse::Click(index) => {
            app.overlay.confirm_selected = index;
            app.overlay.on_confirm_key(&mut app.modals, KeyCode::Enter)
        }
        ChoiceMouse::None => Action::None,
    }
}

/// The stop dialog's keys: Up/Down move the selection, Enter confirms it,
/// Esc closes with no effect, and everything else is swallowed. Ctrl+C is
/// handled before this runs (it detaches from anywhere). The shared
/// choice-dialog key handling moves the selection (finding 6); this owns
/// only the Enter dispatch.
fn stop_key(app: &mut App, key: KeyEvent) -> Action {
    match choice_key(key.code, false, STOP_CHOICES.len(), &mut app.stop.selected) {
        ChoiceKey::Close => app.modals.pop(Modal::Stop),
        ChoiceKey::Enter(index) => {
            app.modals.pop(Modal::Stop);
            return match STOP_CHOICES[index] {
                StopChoice::SoftStop => {
                    app.run.stopping = true;
                    app.status = Some(SOFT_STOP_PENDING.into());
                    Action::Quit
                }
                StopChoice::Interrupt => {
                    app.run.stopping = true;
                    app.status = Some(
                        "Interrupting -- the current task is cancelled and the app keeps running."
                            .into(),
                    );
                    Action::Interrupt
                }
                StopChoice::Cancel => Action::None,
            };
        }
        ChoiceKey::Swallowed => {}
    }
    Action::None
}

/// The stop dialog's mouse handling (T59.1): a click on one of the bottom
/// line's buttons runs exactly that button's key through the dialog's key
/// path -- Enter confirms the selected choice and Esc closes with no
/// effect. A click on the title row's close button runs the dialog's Esc
/// key the same way, leaving the build unchanged (T66.1). A pointer move
/// over one of the dialog's three choice rows moves the selection
/// highlight there without confirming it, and a left click inside a
/// choice row runs that row's Enter key verbatim, so a click and Enter
/// on a row are indistinguishable (T148.1). A move or click inside the
/// dialog that misses every row -- the blank body padding below the
/// choices, the border, the title, the footer's non-button columns --
/// or lands outside it does nothing. Everything else is swallowed,
/// as before.
fn stop_mouse(app: &mut App, mouse: MouseEvent) -> Action {
    let buttons = Modal::Stop.buttons(app);
    match choice_mouse(
        &mouse,
        app.stop.close.get(),
        app.stop.footer.get(),
        &buttons,
        app.stop.body.get(),
        STOP_CHOICES.len(),
    ) {
        ChoiceMouse::Key(code) => stop_key(app, bare(code)),
        // The move moves only the highlight, never the dispatch.
        ChoiceMouse::Hover(index) => {
            app.stop.selected = index;
            Action::None
        }
        // The click is the row's Enter key verbatim: it moves the
        // selection to the row, then runs the Enter dispatch, so a
        // click and Enter on the same row are indistinguishable.
        ChoiceMouse::Click(index) => {
            app.stop.selected = index;
            stop_key(app, bare(KeyCode::Enter))
        }
        ChoiceMouse::None => Action::None,
    }
}

/// The m menu's keys: Up/Down -- j/k aliasing them, the theme picker's
/// convention -- move the selection, Enter runs the selected
/// entry -- exactly the action its direct key binding triggers -- Esc
/// closes with no effect, and everything else is swallowed. Ctrl+C is
/// handled before this runs (it detaches from anywhere). The Enter
/// entries are replacements, not stacks: the menu closes first, then
/// the target opens (see [`ModalStack`]).
fn menu_key(app: &mut App, key: KeyEvent) -> Action {
    let entries = menu_entries(app.run.phase == Phase::Running);
    match key.code {
        KeyCode::Esc => app.modals.pop(Modal::Menu),
        KeyCode::Up | KeyCode::Char('k') => {
            app.menu.selected = app.menu.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.menu.selected = (app.menu.selected + 1).min(entries.len() - 1);
        }
        KeyCode::Enter => {
            app.modals.pop(Modal::Menu);
            return match entries[app.menu.selected.min(entries.len() - 1)] {
                MenuChoice::Settings => {
                    app.status = None;
                    app.open_settings();
                    Action::None
                }
                MenuChoice::Theme => {
                    app.status = None;
                    app.open_theme();
                    Action::None
                }
                MenuChoice::Detach => Action::Detach,
                MenuChoice::Quit => app.quit_action(),
                MenuChoice::Interactive => {
                    app.status = None;
                    app.open_interactive();
                    Action::None
                }
                MenuChoice::StopBuild => {
                    app.status = None;
                    app.open_stop();
                    Action::None
                }
            };
        }
        _ => {}
    }
    Action::None
}

/// The m menu's mouse handling (T59.1): a click on one of the bottom
/// line's buttons runs exactly that button's key through the menu's key
/// path -- Enter runs the selected entry and Esc closes with no effect.
/// A click on the title row's close button runs the menu's Esc key the
/// same way (T66.1). A left click inside a rendered entry row moves the
/// selection to that row and runs it -- exactly the `m` plus Enter
/// path (T137.1). The pointer moving onto a rendered entry moves only
/// the selection highlight there, without running it (T140.1), so a
/// click is exactly hover plus Enter; a move inside the modal but
/// outside every row -- or anywhere else -- leaves the selection
/// unchanged. A click on the status bar's m menu chip closes the
/// menu, the second half of the chip's toggle (T133.1). Everything else
/// -- the border, the title, the footer's non-button columns, the shell
/// behind the modal -- is swallowed, as before.
fn menu_mouse(app: &mut App, mouse: MouseEvent) -> Action {
    if chip_clicked(&mouse, app.layout.menu_chip.get()) {
        app.modals.pop(Modal::Menu);
        return Action::None;
    }
    if close_clicked(&mouse, app.menu.close.get()) {
        return menu_key(app, bare(KeyCode::Esc));
    }
    let buttons = Modal::Menu.buttons(app);
    if let Some(code) = footer_button_code(&mouse, app.menu.footer.get(), &buttons) {
        return menu_key(app, bare(code));
    }
    let position = ratatui::layout::Position::new(mouse.column, mouse.row);
    let body = app.menu.body.get();
    let entries = menu_entries(app.run.phase == Phase::Running);
    let visible = entries.len().min(usize::from(body.height));
    let row = menu_row_at(position, body, visible);
    match mouse.kind {
        MouseEventKind::Moved => {
            // The pointer move moves only the highlight, never the
            // dispatch (T140.1): a move that misses every row -- the
            // border, the title, the footer, the shell behind the
            // modal, the running-only stop row's absent slot -- leaves
            // the selection alone.
            if let Some(index) = row {
                app.menu.selected = index;
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(index) = row {
                // The click is the row's Enter key verbatim: it moves the
                // highlight to the row, then runs the Enter dispatch -- so a
                // click and `m` plus Enter on the same row are
                // indistinguishable.
                app.menu.selected = index;
                return menu_key(app, bare(KeyCode::Enter));
            }
        }
        _ => {}
    }
    Action::None
}

/// The interactive picker's keys: Up/Down -- j/k aliasing them -- move the
/// selection over every row, disabled ones included, so the user can read why
/// one is disabled; Enter on an available provider closes the picker and
/// opens that provider's terminal, on a disabled one does nothing; Esc closes
/// with no effect; everything else is swallowed.
fn picker_key(app: &mut App, key: KeyEvent) -> Action {
    let last = app.interactive.providers.len().saturating_sub(1);
    match key.code {
        KeyCode::Esc => app.modals.pop(Modal::InteractivePicker),
        KeyCode::Up | KeyCode::Char('k') => {
            app.interactive.selected = app.interactive.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.interactive.selected = (app.interactive.selected + 1).min(last);
        }
        KeyCode::Enter => {
            if let Some(&(kind, true)) = app.interactive.providers.get(app.interactive.selected) {
                app.modals.pop(Modal::InteractivePicker);
                app.open_terminal(kind);
            }
        }
        _ => {}
    }
    Action::None
}

/// The interactive picker's mouse handling, in the menu's order: the title
/// row's close button and the footer's buttons run their keys through the
/// picker's key path, the pointer moving onto a rendered row moves only the
/// highlight, and a left click on a row selects it and runs its Enter.
/// Everything else is swallowed.
fn picker_mouse(app: &mut App, mouse: MouseEvent) -> Action {
    if close_clicked(&mouse, app.interactive.close.get()) {
        return picker_key(app, bare(KeyCode::Esc));
    }
    let buttons = Modal::InteractivePicker.buttons(app);
    if let Some(code) = footer_button_code(&mouse, app.interactive.footer.get(), &buttons) {
        return picker_key(app, bare(code));
    }
    let position = ratatui::layout::Position::new(mouse.column, mouse.row);
    let body = app.interactive.body.get();
    let visible = app
        .interactive
        .providers
        .len()
        .min(usize::from(body.height));
    let row = menu_row_at(position, body, visible);
    match mouse.kind {
        MouseEventKind::Moved => {
            if let Some(index) = row {
                app.interactive.selected = index;
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(index) = row {
                app.interactive.selected = index;
                return picker_key(app, bare(KeyCode::Enter));
            }
        }
        _ => {}
    }
    Action::None
}

/// The dialog's key path (T154.2): App owns the modal chrome -- Esc
/// closes keeping the text, Ctrl+C detaches, the status line clears and
/// the goal column is forgotten on every key but Up/Down -- and the
/// editor (`DialogEditor`) owns the buffer, the cursor and their motion.
fn dialog_key(app: &mut App, key: KeyEvent, ctrl: bool) -> Action {
    if key.code == KeyCode::Esc {
        app.modals.pop(Modal::Dialog);
        return Action::None;
    }
    if key.code == KeyCode::Char('c') && ctrl {
        return Action::Detach;
    }
    app.dialog.status = None;
    if !matches!(key.code, KeyCode::Up | KeyCode::Down) {
        app.dialog.editor.reset_goal();
    }
    match key.code {
        KeyCode::Char('s') if ctrl => return app.submit_dialog(),
        KeyCode::Char('u') if ctrl => app.dialog.editor.clear(),
        // Enter submits through the same path as the Submit button;
        // an empty input is refused inline. A submit that
        // launches a run closes the dialog; a command error keeps it open.
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
            if app.dialog.editor.text().trim().is_empty() {
                app.dialog.status = Some(app.empty_input_message().into());
            } else {
                return app.submit_dialog();
            }
        }
        _ => app.dialog.editor.on_key(key, ctrl),
    }
    Action::None
}

/// The add-task dialog's mouse handling (T59.1): a click on one of the
/// bottom line's buttons runs exactly that button's key through the
/// dialog's key path -- Enter submits (with the empty-input refusal) and
/// Esc closes keeping the text. A click on the title row's close button
/// runs the dialog's Esc key the same way (T66.1). Everything else is
/// swallowed, as before.
fn dialog_mouse(app: &mut App, mouse: MouseEvent) -> Action {
    if close_clicked(&mouse, app.dialog.close.get()) {
        return dialog_key(app, bare(KeyCode::Esc), false);
    }
    let buttons = Modal::Dialog.buttons(app);
    let code = footer_button_code(&mouse, app.dialog.footer.get(), &buttons);
    match code {
        Some(code) => dialog_key(app, bare(code), false),
        None => Action::None,
    }
}

/// The theme picker's keys (T43.1, T116.1): Up/Down and j/k move the
/// selection through the visible entries -- the group headers and the
/// expanded groups' rows, in list order -- and immediately live-preview
/// it when it lands on a row; a header takes only the highlight and
/// leaves the preview alone. The acting keys follow the selected entry:
/// on a header, Enter/Space toggle the fold and Left folds an expanded
/// one while Right unfolds a folded one (the settings overlay's header
/// rule, h/l aliasing the arrows); on a row, Enter keeps the previewed
/// theme and hands it to the driver for persistence. Esc restores the
/// theme the picker opened with and closes; everything else is swallowed.
fn theme_key(app: &mut App, key: KeyEvent, ctrl: bool) -> Action {
    // h/l alias Left/Right on the headers (the settings overlay's hjkl
    // convention); j/k are matched directly below.
    let code = match key.code {
        KeyCode::Char('h') if !ctrl => KeyCode::Left,
        KeyCode::Char('l') if !ctrl => KeyCode::Right,
        code => code,
    };
    match code {
        KeyCode::Esc => {
            app.theme_modal.preview = None;
            app.modals.pop(Modal::Theme);
        }
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        code => match app.theme_modal.selected_entry() {
            Entry::Header(group) => {
                if header_fold(code, app.theme_modal.expanded[group]).is_some() {
                    app.theme_modal.toggle(group);
                }
            }
            // Enter on a theme row commits; every other key is swallowed.
            Entry::Row(_) if code == KeyCode::Enter && !ctrl => return app.commit_theme(),
            Entry::Row(_) => {}
        },
    }
    Action::None
}

/// The theme picker's mouse handling (T43.1, T116.1): the pointer moving
/// onto a visible entry moves the selection there and previews it when it
/// is a row -- a header takes only the highlight -- and a click on a row
/// previews and then commits, exactly like hovering plus Enter, while a
/// click on a header toggles its fold and commits nothing. A click on one
/// of the footer's buttons runs that button's key (T59.1), and a click on
/// the title row's close button runs the picker's Esc key -- the previewed
/// theme is dropped and the opening one restored (T66.1). Events outside
/// the visible entries and every other kind are swallowed, so nothing
/// underneath scrolls or focuses.
fn theme_mouse(app: &mut App, mouse: MouseEvent) -> Action {
    if close_clicked(&mouse, app.theme_modal.close.get()) {
        return theme_key(app, bare(KeyCode::Esc), false);
    }
    let position = ratatui::layout::Position::new(mouse.column, mouse.row);
    let row = theme_row_at(
        position,
        app.theme_modal.area.get(),
        app.theme_modal.entries().len(),
    );
    match mouse.kind {
        MouseEventKind::Moved => {
            if let Some(row) = row {
                app.preview_entry(row);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // The footer's buttons sit outside the body rows, so they are
            // checked first (T59.1).
            let buttons = Modal::Theme.buttons(app);
            if let Some(code) = footer_button_code(&mouse, app.theme_modal.footer.get(), &buttons) {
                return theme_key(app, bare(code), false);
            }
            if let Some(row) = row {
                match app.theme_modal.entries()[row] {
                    // A header click folds its group; it never commits.
                    Entry::Header(group) => app.theme_modal.toggle(group),
                    Entry::Row(_) => {
                        app.preview_entry(row);
                        return app.commit_theme();
                    }
                }
            }
        }
        _ => {}
    }
    Action::None
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use patok_core::config::SettingValue;
    use patok_core::event::{Phase, Snapshot};
    use patok_core::pipeline::PipelineState;

    /// Every modal, in no meaningful order; the stack test matrix iterates it.
    const ALL_MODALS: [Modal; 8] = [
        Modal::Overlay,
        Modal::Dialog,
        Modal::Menu,
        Modal::Stop,
        Modal::Confirm,
        Modal::Theme,
        Modal::InteractivePicker,
        Modal::Terminal,
    ];

    /// A minimal idle app, the pattern the other in-crate test modules use.
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
                recent: vec![],
            },
            "0.1.0".into(),
        )
    }

    fn press(app: &mut App, code: KeyCode) -> Action {
        app.on_key(bare(code))
    }

    /// The one legitimate combination stacks in the right order through the
    /// real flow: a dirty overlay's Esc opens the confirm above it, its
    /// cancel returns to the overlay, and closing the overlay dismisses the
    /// confirm stacked on it.
    #[test]
    fn the_confirm_stacks_over_the_overlay_in_that_order() {
        let mut app = app();
        app.open_settings();
        app.overlay
            .drafts
            .insert("builder_model".into(), SettingValue::Str("opus".into()));
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert_eq!(app.modals.top(), Some(Modal::Confirm));
        assert_eq!(
            app.open_modals(),
            vec![Modal::Overlay, Modal::Confirm],
            "the confirm sits above the overlay, the render order's top layer"
        );
        // Esc on the confirm cancels: back to the overlay, drafts intact.
        assert_eq!(press(&mut app, KeyCode::Esc), Action::None);
        assert_eq!(app.open_modals(), vec![Modal::Overlay]);
        assert!(app.overlay.dirty());
        // Closing the overlay through its Discard choice dismisses the
        // confirm stacked on it too: the whole stack goes empty.
        assert_eq!(press(&mut app, KeyCode::Char('q')), Action::None);
        assert_eq!(press(&mut app, KeyCode::Down), Action::None);
        assert_eq!(press(&mut app, KeyCode::Enter), Action::CloseSettings);
        assert!(app.modals.is_empty());
    }

    /// Every other pair is refused: the matrix over all first/second
    /// combinations, with the overlay's confirm the only stack.
    #[test]
    fn every_other_pair_is_a_refused_no_op() {
        for first in ALL_MODALS {
            // The confirm is not a modal of its own: it never opens on an
            // empty stack.
            let mut stack = ModalStack::new();
            if first == Modal::Confirm {
                assert!(!stack.push(first), "the confirm never opens alone");
                continue;
            }
            assert!(
                stack.push(first),
                "the first modal opens on the empty stack"
            );
            for second in ALL_MODALS {
                if first == Modal::Overlay && second == Modal::Confirm {
                    assert!(stack.push(second), "the one legitimate pair");
                    assert_eq!(stack.top(), Some(Modal::Confirm));
                    assert_eq!(
                        stack.iter().collect::<Vec<_>>(),
                        vec![Modal::Overlay, Modal::Confirm]
                    );
                    stack.pop(Modal::Confirm);
                    continue;
                }
                assert!(
                    !stack.push(second),
                    "{first:?} then {second:?} must not stack"
                );
                assert_eq!(stack.top(), Some(first), "{first:?} stays alone");
                assert_eq!(stack.iter().collect::<Vec<_>>(), vec![first]);
            }
        }
    }

    /// A refused open is a no-op: none of the refused modal's state is
    /// touched, so a refused reopen keeps the first open modal exactly as
    /// it was.
    #[test]
    fn a_refused_open_leaves_the_state_untouched() {
        let mut app = app();
        app.open_theme();
        app.theme_modal.selected = 3;
        let before = (
            app.theme_modal.selected,
            app.theme_modal.expanded,
            app.status.clone(),
        );
        // Every other opener is refused while the theme picker is open,
        // and none of them touches their own state.
        app.open_theme();
        app.open_menu();
        app.open_stop();
        app.open_settings();
        app.open_inject();
        assert_eq!(
            before,
            (
                app.theme_modal.selected,
                app.theme_modal.expanded,
                app.status.clone()
            ),
            "the picker's state and the status line are untouched"
        );
        assert_eq!(app.open_modals(), vec![Modal::Theme]);
        assert!(app.overlay.drafts.is_empty());
        assert_eq!(app.menu.selected, 0);
    }

    /// The interactive picker obeys the same rule: opened over another modal
    /// it is refused and computes nothing.
    #[test]
    fn a_refused_interactive_open_leaves_the_picker_empty() {
        let mut app = app();
        app.open_theme();
        app.open_interactive();
        assert_eq!(app.open_modals(), vec![Modal::Theme]);
        assert!(app.interactive.providers.is_empty());
    }

    /// Popping the overlay also removes the confirm above it, the stack's
    /// twin of the old `close` clearing both flags.
    #[test]
    fn popping_the_overlay_removes_the_confirm_above_it() {
        let mut stack = ModalStack::new();
        stack.push(Modal::Overlay);
        stack.push(Modal::Confirm);
        stack.pop(Modal::Overlay);
        assert!(stack.is_empty());
        // A pop of a modal not on the stack is a no-op.
        stack.pop(Modal::Overlay);
        assert!(stack.is_empty());
    }
}
