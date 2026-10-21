//! The sessions (section 12 of docs/private/engine-restructure.md): one
//! session is one engine-owning unit of work, its [`SessionKind`] says
//! which pipeline runs, and every engine-side path is reached through
//! the session context, never the engine directly. The build session
//! loops the task list's pending tasks through [`BuildPipeline`] -- with
//! the continuous-mode discovery round when the queue runs empty -- and
//! the append kinds (add-tasks, queue creation, discovery) run their
//! pipeline once.

use std::path::PathBuf;
use std::time::Instant;

use patok_config::RunMode;
use patok_core::event::TaskOutcome;
use patok_core::task::Task;

use crate::context::{Pause, SessionContext};
use crate::pipeline::add_task::AddTaskPipeline;
use crate::pipeline::build::BuildPipeline;
use crate::pipeline::discovery::DiscoveryPipeline;
use crate::pipeline::queue_creation::{QueueCreation, QueueCreationPipeline};

/// One engine-owning unit of work: the kind says which one runs, and the
/// context holds the settings snapshot and cancel token it runs on. The
/// `start_*` commands construct it after their guards and spawn its
/// [`Session::run`].
pub(crate) struct Session {
    kind: SessionKind,
    ctx: SessionContext,
}

/// A guard that emits SessionClose when dropped.
/// This ensures SessionClose is emitted on all exit paths from Session::run.
pub(crate) struct SessionCloseGuard {
    session_path: Option<PathBuf>,
    inner: std::sync::Arc<crate::engine::Inner>,
}

impl Drop for SessionCloseGuard {
    fn drop(&mut self) {
        if self.session_path.is_some() {
            let engine = crate::Engine {
                inner: self.inner.clone(),
            };
            let mut state = engine.inner.state.lock().expect("engine state lock");
            engine.emit_history_msg_locked(&mut state, crate::history::HistoryMsg::SessionClose);
        }
    }
}

/// Which unit of work the session owns.
pub(crate) enum SessionKind {
    /// The build session loop, starting with the task `start_build` already
    /// announced.
    Build { first: Task },
    /// One planner session appending tasks for the request.
    AddTasks {
        request: String,
        before: String,
        start: Instant,
    },
    /// One research queue-creation run of the given kind.
    QueueCreation {
        kind: QueueCreation,
        before: String,
        start: Instant,
    },
    /// One standalone discovery round.
    Discovery { before: String },
}

impl Session {
    pub(crate) fn new(kind: SessionKind, ctx: SessionContext) -> Self {
        Self { kind, ctx }
    }

    /// Runs the session's unit of work to its end, dispatching on the kind:
    /// the build session loop, one planner run, one queue-creation run or
    /// one standalone discovery round.
    pub(crate) async fn run(self) {
        let Self { kind, ctx } = self;

        // Create a guard that will emit SessionClose on drop (all exit paths)
        let _guard = SessionCloseGuard {
            session_path: ctx.session_path().map(|p| p.to_path_buf()),
            inner: ctx.inner_clone(),
        };

        match kind {
            SessionKind::Build { first } => Self::run_build(&ctx, first).await,
            SessionKind::AddTasks {
                request,
                before,
                start,
            } => {
                AddTaskPipeline::new(&ctx)
                    .run(&request, &before, start)
                    .await;
                ctx.planning_ended();
            }
            SessionKind::QueueCreation {
                kind,
                before,
                start,
            } => {
                QueueCreationPipeline::new(&ctx)
                    .run(kind, &before, start)
                    .await;
                ctx.planning_ended();
            }
            SessionKind::Discovery { before } => {
                let added = Self::run_discovery_round(&ctx, &before).await;
                ctx.record_round(added);
            }
        }
    }

    /// The session-level build loop: pending tasks are
    /// built one after another until none remain, then in continuous mode one discovery
    /// round runs if the cooldown has elapsed; a round that appends tasks continues the
    /// session with them. In sprint mode the empty queue ends the session (the mode comes
    /// from the session's snapshot, so a change takes effect at the next start).
    /// A task that does not complete ends the session (it stays pending, so re-picking it
    /// immediately would loop), as does a stop request. `first` is the task `start_build`
    /// already announced: it runs even when a stop request arrives before the loop does,
    /// so the stop cancels the running agent as before.
    /// Pauses: the inter-task pause runs before every task after the first, the inter-cycle
    /// pause before each scheduled discovery round, and the agent pause before every agent
    /// after the first of a task (in `run_stage`).
    async fn run_build(ctx: &SessionContext, first: Task) {
        if Self::run_task(ctx, first).await != TaskOutcome::Done {
            ctx.build_ended();
            return;
        }
        loop {
            while !ctx.stop_pending() {
                // The inter-task pause runs only when another task is waiting.
                if ctx.next_pending().is_none() {
                    break;
                }
                ctx.pause(Pause::Task).await;
                if ctx.stop_pending() {
                    break;
                }
                let Some(task) = Self::begin_next_task(ctx) else {
                    break;
                };
                if Self::run_task(ctx, task).await != TaskOutcome::Done {
                    ctx.build_ended();
                    return;
                }
            }
            // The queue is empty: sprint mode stops here; in continuous mode one
            // discovery round runs, but only past the cooldown.
            if ctx.stop_pending()
                || ctx.settings().run_mode != RunMode::Continuous
                || !ctx.discovery_due()
            {
                break;
            }
            // The inter-cycle pause before the scheduled discovery round.
            ctx.pause(Pause::Cycle).await;
            if ctx.stop_pending() {
                break;
            }
            match Self::run_scheduled_discovery(ctx).await {
                Some(added) if added > 0 => {} // the round filled the queue: keep going
                _ => break,
            }
        }
        ctx.build_ended();
    }

    /// Picks the first pending task, marks it current and announces it; `None`
    /// when none remain (a task added externally mid-session is picked up here
    /// like any other).
    fn begin_next_task(ctx: &SessionContext) -> Option<Task> {
        let task = ctx.next_pending()?;
        ctx.begin_task(&task);
        Some(task)
    }

    /// Runs one task's build session: [`BuildPipeline`] owns the task's
    /// per-task build flow -- the research, plan, build, review and commit
    /// steps, their order, every tile update, the progress token and the
    /// batch-review group wiring -- on the session's settings snapshot in
    /// the context. This ends the task through the context's
    /// `finish_task`, which emits `TaskFinished` with the outcome; the
    /// session-level loop decides what happens next.
    async fn run_task(ctx: &SessionContext, task: Task) -> TaskOutcome {
        let (outcome, commit) = BuildPipeline::new(ctx).run(&task).await;
        ctx.finish_task(&task, outcome, commit);
        outcome
    }

    /// One scheduled discovery round at the empty end of a session: like the
    /// manual round, the file is recorded first -- under the task-file lock
    /// only for that read, released before the session runs (T77.1) and
    /// re-acquired for the post-session check. It runs on the session's
    /// settings snapshot. Returns how many tasks it appended, or `None`
    /// when no round ran to completion.
    async fn run_scheduled_discovery(ctx: &SessionContext) -> Option<usize> {
        let before = ctx.read_before().await?;
        ctx.gap_discovery_started();
        let added = Self::run_discovery_round(ctx, &before).await;
        ctx.record_round(added);
        added
    }

    /// One discovery round: [`DiscoveryPipeline`] owns the round's
    /// flow -- the discovery agent session and, when it completed, the
    /// post-session append validation -- on the session's settings
    /// snapshot in the context. `before` is the task file as it was before
    /// the round, with the task-file lock free for the round's whole
    /// duration (T77.1). Returns how many tasks the round appended once it
    /// completes, or `None` when it did not run to completion (error,
    /// cancelled or failed); only completed rounds move the schedule. A
    /// round whose session completed but was rejected still counts as
    /// completed, with zero tasks appended, so the schedule records it.
    async fn run_discovery_round(ctx: &SessionContext, before: &str) -> Option<usize> {
        let added = DiscoveryPipeline::new(ctx).run(before).await;
        ctx.discovery_ended(added);
        added
    }
}
