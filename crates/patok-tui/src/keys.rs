//! Key input (finding 19, the rest, in
//! docs/private/structure-review-followup.md): the main view's key handler
//! and the quit and run-mode helpers behind it, split out of `app`. The open
//! modal stack swallows every key first; the modals' own key handling lives
//! in `modal`.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use patok_core::event::Phase;

use crate::app::{Action, App, SOFT_STOP_PENDING};
use crate::modal::{Modal, ModalLayer};
use crate::view::FrameFocus;

/// Shift+letter from terminals that report the base key (kitty without
/// alternate keys, some modifyOtherKeys setups) arrives as the lowercase
/// char plus SHIFT; every text input and shortcut wants the char the user
/// typed, so it becomes the uppercase char. Ctrl and Alt chords keep their
/// base key, and chars without a single-char uppercase form pass through.
fn shifted_char(key: KeyEvent) -> KeyEvent {
    let KeyCode::Char(c) = key.code else {
        return key;
    };
    if !key.modifiers.contains(KeyModifiers::SHIFT)
        || key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        || !c.is_lowercase()
    {
        return key;
    }
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) => KeyEvent {
            code: KeyCode::Char(u),
            ..key
        },
        _ => key,
    }
}

impl App {
    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        // Runs before the terminal, modal and main-view dispatch, so every
        // text field and shortcut sees the same char.
        let key = shifted_char(key);
        self.run.new_tasks.clear();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // The embedded terminal takes every key first: Ctrl+C belongs to its
        // child, and Ctrl+] closes it.
        if self.modals.top() == Some(Modal::Terminal) {
            return self.terminal_key(key);
        }
        // Ctrl+C detaches from anywhere else, overlay and dialog included.
        if key.code == KeyCode::Char('c') && ctrl {
            return Action::Detach;
        }
        // The open modal swallows every other key, so nothing underneath
        // reacts -- no restart, no navigation, no second build. The top of
        // the modal stack is the one modal that wins; the mouse dispatch and
        // the render order read the same stack (finding 20). Ctrl+C above
        // still detaches, the same convention as before.
        if let Some(modal) = self.modals.top() {
            return modal.on_key(self, key, ctrl);
        }
        match key.code {
            KeyCode::Char('d') => Action::Detach,
            // The theme picker's key (T43.1): it opens from any state except
            // while another modal has the keyboard (the guards above), and it
            // sits above even a pending soft stop, whose q/Esc keys work
            // again once the picker closes.
            KeyCode::Char('t') if !ctrl => {
                self.status = None;
                self.open_theme();
                Action::None
            }
            // The m menu's key: it opens from any engine state except while
            // another modal has the keyboard (the guards above), so the
            // status bar's secondary hints (settings, theme, detach, quit and
            // the running Esc stop dialog) live behind one chip instead.
            KeyCode::Char('m') if !ctrl => {
                self.status = None;
                self.open_menu();
                Action::None
            }
            // The interactive picker's key (T2.1): Shift+i, distinct from the
            // task frame's `i` inject key (`shifted_char` turns a kitty
            // `i`+SHIFT into `I` on every terminal); like `m` it opens from any engine
            // state, a pending soft stop included, so it sits above the
            // stopping guard.
            KeyCode::Char('I') if !ctrl => {
                self.status = None;
                self.open_interactive();
                Action::None
            }
            // S-Tab flips the run mode between `sprint` and `continuous`
            // (T60.1): the new value goes through the settings-change flow, so
            // the status line's chip follows as soon as the engine reports it, while
            // an already running loop keeps its snapshot. Sits above the plain
            // Tab arm so a shifted Tab never switches the frame focus.
            KeyCode::BackTab => self.flip_run_mode(),
            // Kitty-enhanced terminals report Shift plus Tab as a Tab with the
            // SHIFT modifier instead of BackTab; both encode the same key.
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => self.flip_run_mode(),
            // Tab switches the frame focus; the layout resizes around it.
            KeyCode::Tab => {
                self.focus = match self.focus {
                    FrameFocus::Output => FrameFocus::Tasks,
                    FrameFocus::Tasks => FrameFocus::Output,
                };
                Action::None
            }
            _ if self.run.stopping => match key.code {
                // A second q interrupts: the current task is cancelled and the
                // loop stops, but the app keeps running.
                KeyCode::Char('q') => Action::Interrupt,
                // Esc cancels a pending soft stop: the loop continues with the
                // next task once the running one finishes.
                KeyCode::Esc => {
                    self.run.stopping = false;
                    self.status = Some("Soft stop cancelled -- the build continues.".into());
                    Action::CancelSoftStop
                }
                _ => Action::None,
            },
            KeyCode::Char('q') => self.quit_action(),
            KeyCode::Char('r') if self.version_mismatch => {
                self.run.stopping = true;
                self.status = Some(
                    "Restarting the engine -- it finishes the current task first. [d] detach instead"
                        .into(),
                );
                Action::Restart
            }
            // Esc while a build, planner or discovery run is active opens
            // the stop dialog (T46.1, the key the `s` of T20.1 used to be,
            // extended to these runs in T142.1); the m menu's stop entry
            // keeps its build-only scope. The stopping guard above keeps a
            // pending soft stop's Esc.
            KeyCode::Esc
                if self.run.phase == Phase::Running
                    || self.run.planning
                    || self.run.discovering =>
            {
                self.status = None;
                self.open_stop();
                Action::None
            }
            // The add-task dialog's key (T74.1): it belongs to the task list
            // frame, so only that frame's focused state opens the dialog; the
            // agent output frame ignores it and the key falls through to the
            // no-op arm below, leaving the status line untouched.
            KeyCode::Char('a') if self.focus == FrameFocus::Tasks => self.open_dialog(),
            // The inject-task modal's key (T76.1): like `a`, it belongs to the
            // task list frame and only that frame's focused state opens the
            // modal; the agent output frame ignores it. Unlike `a`, no busy
            // state refuses it -- appending a line is safe in every engine
            // state, and the engine's task-file poll reconciles it.
            KeyCode::Char('i') if !ctrl && self.focus == FrameFocus::Tasks => self.open_inject(),
            // Enter is the primary action while the engine is idle (T46.1):
            // it starts the build loop while pending tasks remain and runs a
            // discovery round once the queue is complete.
            KeyCode::Enter if !ctrl => self.enter_key(),
            KeyCode::Char('?') if !ctrl => {
                self.status = None;
                self.open_settings();
                Action::None
            }
            // The task cursor's keys (T4.1): the focused task list moves its
            // selection; the output frame keeps scrolling with them below.
            KeyCode::Up | KeyCode::Char('k') if self.focus == FrameFocus::Tasks => {
                self.move_task_cursor(-1)
            }
            KeyCode::Down | KeyCode::Char('j') if self.focus == FrameFocus::Tasks => {
                self.move_task_cursor(1)
            }
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(-1),
            KeyCode::PageUp => self.scroll_by(10),
            KeyCode::PageDown => self.scroll_by(-10),
            KeyCode::End => {
                self.scroll = 0;
                Action::None
            }
            _ => Action::None,
        }
    }

    /// The quit action (the `q` key and the m menu's Quit entry, one shared
    /// path): while a run is active the first press soft-stops the build loop
    /// -- the current task finishes, no further task starts, and the app
    /// keeps running; Esc cancels the stop and a second press interrupts.
    /// Idle, nothing needs winding down, so it stops the app directly. A
    /// press while a soft stop is already pending interrupts instead; the
    /// main match's stopping guard reaches the `q` key before this helper,
    /// so that branch serves the menu's path, where no guard runs first.
    pub(crate) fn quit_action(&mut self) -> Action {
        if self.run.stopping {
            return Action::Interrupt;
        }
        self.run.stopping = true;
        if self.run.phase == Phase::Running || self.run.planning || self.run.discovering {
            self.status = Some(SOFT_STOP_PENDING.into());
            Action::Quit
        } else {
            self.status = Some("Stopping the engine...".into());
            Action::Interrupt
        }
    }

    /// Flips the run mode to its other value (S-Tab, T60.1): the newly selected
    /// mode is named in the status bar and carried by the returned action, which
    /// the driver sends through the settings-change flow. The chip follows the
    /// engine-reported readout, so it updates once the change applies; a running
    /// loop keeps its snapshot until its next unit of work.
    fn flip_run_mode(&mut self) -> Action {
        let mode = if self.run_mode() == "sprint" {
            "continuous"
        } else {
            "sprint"
        };
        self.status = Some(format!("Run mode switched to {mode}."));
        Action::SetRunMode(mode.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use patok_core::event::Snapshot;
    use patok_core::pipeline::PipelineState;

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

    fn ev(c: char, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), modifiers)
    }

    #[test]
    fn shifted_char_normalization() {
        let shifted = |c, m| shifted_char(ev(c, m)).code;
        assert_eq!(shifted('a', KeyModifiers::SHIFT), KeyCode::Char('A'));
        assert_eq!(shifted('A', KeyModifiers::SHIFT), KeyCode::Char('A'));
        assert_eq!(shifted('!', KeyModifiers::SHIFT), KeyCode::Char('!'));
        assert_eq!(shifted('a', KeyModifiers::NONE), KeyCode::Char('a'));
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(shifted('a', ctrl_shift), KeyCode::Char('a'));
        let alt_shift = KeyModifiers::ALT | KeyModifiers::SHIFT;
        assert_eq!(shifted('a', alt_shift), KeyCode::Char('a'));
        assert_eq!(shifted('ß', KeyModifiers::SHIFT), KeyCode::Char('ß'));
        assert_eq!(shifted('é', KeyModifiers::SHIFT), KeyCode::Char('É'));
    }

    #[test]
    fn unshifted_j_k_still_scroll() {
        let mut app = app();
        app.focus = FrameFocus::Output;
        app.layout.max_scroll.set(10);
        app.on_key(ev('k', KeyModifiers::NONE));
        assert_eq!(app.scroll, 1);
        app.on_key(ev('j', KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn shift_letters_do_not_trigger_lowercase_shortcuts() {
        let mut app = app();
        app.focus = FrameFocus::Output;
        app.layout.max_scroll.set(10);
        app.on_key(ev('k', KeyModifiers::SHIFT));
        assert_eq!(app.scroll, 0);
        app.on_key(ev('j', KeyModifiers::SHIFT));
        assert_eq!(app.scroll, 0);
        assert_eq!(app.on_key(ev('q', KeyModifiers::SHIFT)), Action::None);
        assert!(!app.run.stopping);
        app.focus = FrameFocus::Tasks;
        app.on_key(ev('a', KeyModifiers::SHIFT));
        assert!(app.modals.is_empty());
        app.on_key(ev('i', KeyModifiers::SHIFT));
        assert_ne!(app.modals.top(), Some(Modal::Dialog));
    }
}
