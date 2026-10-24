//! The interactive shell's shared view vocabulary (finding 19, the rest, in
//! docs/private/structure-review-followup.md): the merged view's frame focus
//! and the stop and delete dialogs' choices, defined outside the shell state (`app`) so
//! the state, the hit-tests (`hit`) and the renderers (`ui`) all read them
//! without importing the state back.

use crate::choice::Choice;

/// The merged view's focused frame: which of the two stacked frames takes all the
/// free space (the other shrinks to 5 content lines).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameFocus {
    /// The agent output frame.
    Output,
    /// The task list frame.
    Tasks,
}

/// The stop dialog's choices (Esc while a build runs, T46.1): what the three
/// rows do, in the order they render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopChoice {
    /// The SOFT-scope shutdown: finish the current task, then stop the loop.
    SoftStop,
    /// The NOW-scope shutdown: cancel the running task and stop the loop
    /// without waiting; the app keeps running.
    Interrupt,
    /// Close the dialog; the build keeps running unchanged.
    Cancel,
}

impl StopChoice {
    pub fn label(self) -> &'static str {
        match self {
            StopChoice::SoftStop => "Soft stop",
            StopChoice::Interrupt => "Interrupt",
            StopChoice::Cancel => "Cancel",
        }
    }

    /// The one-line explanation rendered after the label.
    pub fn detail(self) -> &'static str {
        match self {
            StopChoice::SoftStop => "finish the current task, then stop",
            StopChoice::Interrupt => "cancel the current task, stay open",
            StopChoice::Cancel => "keep the build running",
        }
    }
}

/// The stop dialog's rows through the shared choice-dialog component
/// (finding 6): the inherent `label` and `detail` above stay the public
/// surface; the trait only forwards to them.
impl Choice for StopChoice {
    fn label(&self) -> &'static str {
        (*self).label()
    }

    fn detail(&self) -> &'static str {
        (*self).detail()
    }
}

/// The stop dialog's rows, top to bottom; the stop dialog's selected row (`StopState::selected`) indexes this list.
pub const STOP_CHOICES: [StopChoice; 3] = [
    StopChoice::SoftStop,
    StopChoice::Interrupt,
    StopChoice::Cancel,
];

/// The delete dialog's choices (`d` on a pending task, T7.2), in render order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteChoice {
    /// Remove the task from TASKS.md.
    Delete,
    /// Close the dialog; the task stays.
    Cancel,
}

impl DeleteChoice {
    pub fn label(self) -> &'static str {
        match self {
            DeleteChoice::Delete => "Delete",
            DeleteChoice::Cancel => "Cancel",
        }
    }

    /// The one-line explanation rendered after the label.
    pub fn detail(self) -> &'static str {
        match self {
            DeleteChoice::Delete => "remove the task from TASKS.md",
            DeleteChoice::Cancel => "keep the task",
        }
    }
}

/// The delete dialog's rows through the shared choice-dialog component; the
/// trait only forwards to the inherent methods above.
impl Choice for DeleteChoice {
    fn label(&self) -> &'static str {
        (*self).label()
    }

    fn detail(&self) -> &'static str {
        (*self).detail()
    }
}

/// The delete dialog's rows, top to bottom; its selected row (`DeleteState::selected`) indexes this list.
pub const DELETE_CHOICES: [DeleteChoice; 2] = [DeleteChoice::Delete, DeleteChoice::Cancel];
