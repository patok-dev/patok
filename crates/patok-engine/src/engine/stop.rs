use patok_core::event::{EngineEvent, NoticeLevel};
use tokio::sync::Notify;

use super::Engine;

/// A pending stop: no new unit of work starts. A soft stop (`Soft`) is
/// cancellable by [`Engine::cancel_soft_stop`]; a NOW stop (`Now`) already
/// cancelled the running agent and is not. Both are released by
/// [`Engine::end_stop`] once the loop has wound down; the process exits only
/// for a NOW stop requested while idle (the plain quit), a signal, or the
/// idle timeout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Stop {
    #[default]
    None,
    /// A soft stop is pending.
    Soft,
    /// A NOW stop is pending.
    Now,
}

impl Stop {
    /// The stop a request marks: a later request overwrites an earlier one,
    /// so a soft request after a NOW stop makes the stop cancellable again.
    fn requested(now: bool) -> Self {
        if now { Self::Now } else { Self::Soft }
    }

    /// Whether a stop is pending: no new unit of work starts.
    pub(crate) fn is_pending(self) -> bool {
        self != Self::None
    }

    /// Whether the pending stop is a cancellable soft stop.
    pub(crate) fn is_soft(self) -> bool {
        self == Self::Soft
    }
}

impl Engine {
    /// Requests a stop: no new unit of work starts. A running task finishes first
    /// unless `now`, which kills the agent. Neither scope quits the process: a
    /// soft stop (`now` false) and a NOW stop on a busy engine are both
    /// interrupt-stops -- the engine returns to its idle state and keeps serving
    /// once [`Engine::end_stop`] releases the stop. The process exits only for a
    /// NOW stop requested while idle (the plain quit), a signal, or the idle
    /// timeout, which cancel the shutdown token directly.
    pub fn request_stop(&self, now: bool) {
        let mut state = self.state();
        state.stop = Stop::requested(now);
        if now {
            state.cancel.cancel();
        }
    }

    /// Releases a stop once the loop has wound down (the running task finished
    /// or was cancelled, and nothing new started): new units of work --
    /// StartBuild, AddTasks, RunDiscovery -- are accepted again. Both scopes
    /// end here; only the shutdown handshake calls this. The stop stays set
    /// until the engine has actually gone idle, otherwise a command could
    /// sneak a new run in mid-wind-down.
    pub fn end_stop(&self) {
        let mut state = self.state();
        state.stop = Stop::None;
    }

    /// Notified when a pending soft stop is cancelled; the shutdown handshake
    /// selects on this so its stream ends with a cancelled update instead of
    /// lingering until the whole build finishes.
    pub fn soft_stop_cancelled(&self) -> &Notify {
        &self.inner.soft_stop_cancelled
    }

    /// Cancels a pending soft stop: the build loop continues with the next
    /// pending task once the running task finishes, and the engine returns to
    /// normal RUNNING operation. The state change is broadcast to attached
    /// shells as an info notice. Errors are user-facing command errors, for
    /// the `CancelSoftStop` command with no soft stop pending.
    pub fn cancel_soft_stop(&self) -> Result<(), String> {
        let mut state = self.state();
        if !state.stop.is_soft() {
            return Err("no soft stop is pending".into());
        }
        state.stop = Stop::None;
        self.emit_locked(
            &mut state,
            EngineEvent::Notice {
                level: NoticeLevel::Info,
                text: "Soft stop cancelled -- the build loop continues with the next task.".into(),
            },
        );
        drop(state);
        self.inner.soft_stop_cancelled.notify_waiters();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_distinguish_a_soft_stop_from_an_interrupt() {
        // A soft stop is pending and cancellable.
        assert!(Stop::requested(false).is_pending());
        assert!(Stop::requested(false).is_soft());
        // A NOW stop is pending but not cancellable.
        assert!(Stop::requested(true).is_pending());
        assert!(!Stop::requested(true).is_soft());
        // No stop is neither.
        assert!(!Stop::None.is_pending());
        assert!(!Stop::None.is_soft());
        // The last request wins: a soft request after a NOW stop makes the
        // stop cancellable again, like the old `soft_stop = !now` overwrite.
        let mut stop = Stop::requested(true);
        assert_eq!(stop, Stop::Now);
        stop = Stop::requested(false);
        assert_eq!(stop, Stop::Soft);
        assert!(stop.is_soft());
    }
}
