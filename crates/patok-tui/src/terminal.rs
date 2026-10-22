//! The embedded provider terminal (T2.3, T2.4): the state behind
//! `Modal::Terminal` and the `App` methods that open it, feed it keys, mouse
//! and paste, keep its size in step with the screen and close it. An exited
//! child keeps its final screen until the user closes. The PTY runtime itself
//! lives in `interactive`.

use std::cell::Cell;
use std::ffi::{OsStr, OsString};

use crossterm::event::{KeyEvent, MouseEvent};
use patok_providers::{INTERACTIVE_TERMINAL_ENV, ProviderKind, strip_from_interactive_env};
use portable_pty::CommandBuilder;
use ratatui::layout::Rect;
use tokio::sync::mpsc::Receiver;

use crate::app::{Action, App};
use crate::hit::{close_clicked, footer_button_rects};
use crate::interactive::{
    InputModes, KeyInput, PtyEvent, PtySession, encode_key, encode_mouse, encode_paste,
};
use crate::modal::{Modal, ModalLayer};
use crate::ui::terminal_body;

/// The size a PTY gets when nothing has been drawn yet.
const FALLBACK_SIZE: (u16, u16) = (24, 80);

/// The open terminal: the session and the rects its mouse handling reads.
pub(crate) struct TerminalState {
    pub(crate) kind: ProviderKind,
    pub(crate) session: PtySession,
    /// Handed to the event loop by [`App::take_terminal_events`]; `None`
    /// afterwards.
    events: Option<Receiver<PtyEvent>>,
    /// Set once the child has exited; the final screen stays up.
    pub(crate) exited: bool,
    /// The child's exit code, once it has exited.
    pub(crate) exit_code: Option<u32>,
    /// The PTY's rect at the last render.
    pub(crate) body: Cell<Rect>,
    /// The close button's rect at the last render.
    pub(crate) close: Cell<Rect>,
    /// The footer's rect at the last render.
    pub(crate) footer: Cell<Rect>,
}

/// Removes every host variable in `names` that [`strip_from_interactive_env`]
/// drops from the child's environment.
fn strip_host_env(cmd: &mut CommandBuilder, names: impl IntoIterator<Item = OsString>) {
    for name in names {
        if strip_from_interactive_env(&name) {
            cmd.env_remove(&name);
        }
    }
}

/// The PTY command for `kind` in `project`, with the env built in the order
/// the capability table documents: strip, terminal variables, then the
/// provider's own. `search_path` overrides `PATH` so the launch searches the
/// same path the picker's availability check did.
fn launch_command(
    kind: ProviderKind,
    project: &str,
    search_path: Option<&OsStr>,
) -> CommandBuilder {
    let launch = kind.interactive_launch();
    let mut cmd = CommandBuilder::new(launch.program);
    cmd.args(launch.args);
    cmd.cwd(project);
    strip_host_env(&mut cmd, std::env::vars_os().map(|(name, _)| name));
    if let Some(path) = search_path {
        cmd.env("PATH", path);
    }
    for (name, value) in INTERACTIVE_TERMINAL_ENV {
        cmd.env(name, value);
    }
    for (name, value) in launch.env {
        cmd.env(name, value);
    }
    cmd
}

impl App {
    /// Opens the terminal for `kind` in the project directory. A failed
    /// spawn opens nothing and says why in the status line.
    pub(crate) fn open_terminal(&mut self, kind: ProviderKind) {
        let screen = self.layout.screen.get();
        let body = terminal_body(Modal::Terminal.area(self, screen));
        let (rows, cols) = if body.width == 0 || body.height == 0 {
            FALLBACK_SIZE
        } else {
            (body.height, body.width)
        };
        let cmd = launch_command(kind, &self.project, self.interactive.search_path.as_deref());
        match PtySession::spawn(cmd, rows, cols) {
            Err(e) => {
                self.status = Some(format!(
                    "Interactive mode: could not start {}: {e:#}",
                    kind.info().harness_name
                ));
            }
            Ok((session, events)) => {
                // A refused push drops the session, which kills the child.
                if self.modals.push(Modal::Terminal) {
                    self.terminal = Some(TerminalState {
                        kind,
                        session,
                        events: Some(events),
                        exited: false,
                        exit_code: None,
                        body: Cell::new(Rect::default()),
                        close: Cell::new(Rect::default()),
                        footer: Cell::new(Rect::default()),
                    });
                }
            }
        }
    }

    /// The PTY event receiver of a freshly opened terminal, once: the event
    /// loop owns it, because `select!` cannot borrow it from the app.
    pub fn take_terminal_events(&mut self) -> Option<Receiver<PtyEvent>> {
        self.terminal.as_mut()?.events.take()
    }

    /// Resizes the PTY to the body the renderer will compute for `screen`; a
    /// no-op when nothing is open or the size is unchanged.
    pub fn fit_terminal(&mut self, screen: Rect) {
        let body = terminal_body(Modal::Terminal.area(self, screen));
        let Some(terminal) = self.terminal.as_mut() else {
            return;
        };
        if body.width == 0 || body.height == 0 {
            return;
        }
        if let Err(e) = terminal.session.resize(body.height, body.width) {
            self.status = Some(format!("Interactive mode: could not resize: {e:#}"));
        }
    }

    /// Closes the terminal; dropping the session kills the child's process
    /// group.
    pub(crate) fn close_terminal(&mut self) {
        self.modals.pop(Modal::Terminal);
        self.terminal = None;
    }

    /// The child exited: the final screen stays up. A stale event after the
    /// terminal closed is ignored.
    pub fn on_terminal_exited(&mut self, code: u32) {
        if let Some(terminal) = self.terminal.as_mut() {
            terminal.exited = true;
            terminal.exit_code = Some(code);
        }
    }

    fn terminal_write(&self, bytes: &[u8]) {
        // Once the leader has exited the session is over: input would only
        // reach a surviving grandchild the user cannot see. A full queue or
        // a closed writer also drops the input; the UI never blocks on the
        // child.
        if let Some(terminal) = &self.terminal
            && !terminal.exited
        {
            let _ = terminal.session.write(bytes);
        }
    }

    fn terminal_modes(&self) -> Option<InputModes> {
        let terminal = self.terminal.as_ref()?;
        Some(terminal.session.with_screen(InputModes::from_screen))
    }

    /// One key for the terminal: everything goes to the child except
    /// `Ctrl+]`, which closes it.
    pub(crate) fn terminal_key(&mut self, key: KeyEvent) -> Action {
        let Some(modes) = self.terminal_modes() else {
            return Action::None;
        };
        match encode_key(&key, &modes) {
            KeyInput::Exit => self.close_terminal(),
            KeyInput::Forward(bytes) => self.terminal_write(&bytes),
            KeyInput::Ignore => {}
        }
        Action::None
    }

    /// One mouse event for the terminal: the close button and the footer's
    /// button close it, anything else goes to the child when it tracks the
    /// mouse.
    pub(crate) fn terminal_mouse(&mut self, mouse: MouseEvent) -> Action {
        let Some(terminal) = self.terminal.as_ref() else {
            return Action::None;
        };
        let (close, footer, body) = (
            terminal.close.get(),
            terminal.footer.get(),
            terminal.body.get(),
        );
        let on_button = footer_button_rects(footer, &Modal::Terminal.buttons(self))
            .iter()
            .any(|rect| close_clicked(&mouse, *rect));
        if close_clicked(&mouse, close) || on_button {
            self.close_terminal();
            return Action::None;
        }
        if let Some(modes) = self.terminal_modes()
            && let Some(bytes) = encode_mouse(&mouse, body, &modes)
        {
            self.terminal_write(&bytes);
        }
        Action::None
    }

    /// Pasted text for the terminal, bracketed when the child asked for it.
    pub(crate) fn terminal_paste(&mut self, text: &str) {
        if let Some(modes) = self.terminal_modes() {
            self.terminal_write(&encode_paste(text, &modes));
        }
    }

    /// Whether the terminal modal is open.
    #[cfg(any(test, feature = "test-support"))]
    pub fn terminal_open(&self) -> bool {
        self.terminal.is_some()
    }

    /// Whether the open terminal's child has exited.
    #[cfg(any(test, feature = "test-support"))]
    pub fn terminal_exited(&self) -> bool {
        self.terminal.as_ref().is_some_and(|t| t.exited)
    }

    /// The open terminal's screen text.
    #[cfg(any(test, feature = "test-support"))]
    pub fn terminal_contents(&self) -> Option<String> {
        let terminal = self.terminal.as_ref()?;
        Some(terminal.session.with_screen(|s| s.contents()))
    }

    /// The open terminal's emulator size, (rows, cols).
    #[cfg(any(test, feature = "test-support"))]
    pub fn terminal_size(&self) -> Option<(u16, u16)> {
        let terminal = self.terminal.as_ref()?;
        Some(terminal.session.with_screen(|s| s.size()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(cmd: &CommandBuilder, name: &str) -> Option<String> {
        cmd.get_env(name).map(|v| v.to_string_lossy().into_owned())
    }

    #[test]
    fn the_command_runs_the_provider_in_the_project() {
        let cmd = launch_command(ProviderKind::Claude, "/tmp", None);
        assert_eq!(cmd.get_argv()[0], "claude");
        assert_eq!(
            cmd.get_cwd().map(|c| c.to_string_lossy().into_owned()),
            Some("/tmp".into())
        );
    }

    #[test]
    fn the_terminal_variables_are_set() {
        let cmd = launch_command(ProviderKind::Codex, "/tmp", None);
        assert_eq!(env_of(&cmd, "TERM").as_deref(), Some("xterm-256color"));
        assert_eq!(env_of(&cmd, "COLORTERM").as_deref(), Some("truecolor"));
    }

    #[test]
    fn only_claude_marks_a_nested_session() {
        let claude = launch_command(ProviderKind::Claude, "/tmp", None);
        assert_eq!(env_of(&claude, "CLAUDECODE").as_deref(), Some(""));
        let codex = launch_command(ProviderKind::Codex, "/tmp", None);
        assert_ne!(env_of(&codex, "CLAUDECODE").as_deref(), Some(""));
    }

    #[test]
    fn the_search_path_overrides_path() {
        let cmd = launch_command(
            ProviderKind::Claude,
            "/tmp",
            Some(OsStr::new("/stubs:/bin")),
        );
        assert_eq!(env_of(&cmd, "PATH").as_deref(), Some("/stubs:/bin"));
    }

    #[test]
    fn host_terminal_variables_are_stripped_and_others_kept() {
        let mut cmd = CommandBuilder::new("x");
        cmd.env("NO_COLOR", "1");
        cmd.env("KITTY_WINDOW_ID", "3");
        cmd.env("HOME", "/h");
        strip_host_env(
            &mut cmd,
            ["NO_COLOR", "KITTY_WINDOW_ID", "HOME"].map(OsString::from),
        );
        assert_eq!(env_of(&cmd, "NO_COLOR"), None);
        assert_eq!(env_of(&cmd, "KITTY_WINDOW_ID"), None);
        assert_eq!(env_of(&cmd, "HOME").as_deref(), Some("/h"));
    }
}
