//! The embedded-terminal runtime. The PTY lives in the TUI process because
//! the engine is a remote daemon and the child must draw into this terminal.

mod replies;
mod session;
mod widget;

#[expect(unused_imports, reason = "first used by the Terminal modal (T2.3)")]
pub(crate) use session::{PtyEvent, PtySession};

#[expect(unused_imports, reason = "first used by the Terminal modal (T2.3)")]
pub(crate) use widget::{ScreenView, cursor_position};
