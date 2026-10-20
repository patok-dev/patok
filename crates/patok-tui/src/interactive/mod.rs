//! The embedded-terminal runtime. The PTY lives in the TUI process because
//! the engine is a remote daemon and the child must draw into this terminal.

mod input;
mod replies;
mod session;
mod widget;

pub(crate) use input::{InputModes, KeyInput, encode_key, encode_mouse, encode_paste};

pub use session::PtyEvent;
pub(crate) use session::PtySession;

pub(crate) use widget::{ScreenView, cursor_position};
