use patok_config::RunMode;
use patok_core::event::EngineEvent;
use patok_core::pipeline::{PipelineState, Tile, TileStatus};

use super::Engine;
use super::commands::StartCommand;
use super::state::{RunState, State, Tiles};

impl Engine {
    /// The shared preamble of the four `start_*` commands (finding 17 in
    /// docs/private/structure-review.md): a pending stop or a non-idle run
    /// state rejects the command with the same reasons as before. The caller
    /// holds the state guard, so the ordering against each command's own
    /// pre-checks (the task-file read of `start_build`, the reuse window of
    /// `start_queue_creation`) is unchanged.
    pub(crate) fn check_start(state: &State, command: StartCommand) -> Result<(), String> {
        if state.stop.is_pending() {
            return Err("the engine is shutting down".into());
        }
        match state.run {
            RunState::Running | RunState::SessionDiscovery => {
                Err(command.running_rejection().into())
            }
            RunState::Planning => Err(command.planning_rejection().into()),
            RunState::Discovering => {
                Err("a discovery round is running; wait for it to finish".into())
            }
            RunState::Idle => Ok(()),
        }
    }

    /// Applies `run`: when the wire phase it reports changed, updates the
    /// phase watch and emits PhaseChanged. The behaviour of the old
    /// `set_phase`, driven by the run-state enum instead of a loose flag.
    pub(crate) fn apply_run_state(&self, state: &mut State, run: RunState) {
        let phase = run.phase();
        if state.run.phase() != phase {
            self.inner.phase.send_replace(phase);
            self.emit_locked(state, EngineEvent::PhaseChanged { phase });
        }
        state.run = run;
    }

    /// Re-reads the task file and merges it into the queue; broadcasts the new list when it
    /// changed. Returns whether it did. The change detection lives in the
    /// task list; the running-task id still comes from the held state lock
    /// (state first, task list second).
    pub(crate) fn reconcile_locked(&self, state: &mut State) -> bool {
        let Some(merged) = self
            .inner
            .task_list
            .reconcile(state.current_task.as_deref())
        else {
            return false;
        };
        self.emit_locked(state, EngineEvent::TasksChanged { tasks: merged });
        true
    }

    pub(crate) fn reconcile(&self) -> bool {
        let mut state = self.state();
        self.reconcile_locked(&mut state)
    }

    /// Recomputes the pipeline rail from the
    /// per-task and session inputs and, when it changed, swaps it in and
    /// broadcasts it. The run mode is read from the central settings at the
    /// point of use. Every transition calls this one helper, so the broadcast,
    /// the snapshot and the replayed recent events can never disagree.
    pub(crate) fn refresh_pipeline_locked(&self, state: &mut State) {
        let sprint = self.central().settings.run_mode == RunMode::Sprint;
        let discover = if sprint {
            TileStatus::Muted
        } else if state.run.is_discovering() {
            TileStatus::Active
        } else if state.discovery_ran {
            TileStatus::Done
        } else {
            TileStatus::Muted
        };
        let pipeline = PipelineState {
            stages: state
                .pipeline
                .stages
                .iter()
                .map(|tile| Tile {
                    stage: tile.stage,
                    status: state.tiles.get(tile.stage),
                })
                .collect(),
            discover,
        };
        if pipeline != state.pipeline {
            state.pipeline = pipeline.clone();
            self.emit_locked(state, EngineEvent::PipelineChanged { state: pipeline });
        }
    }

    /// One stage-tile update under an already-held state lock, followed by
    /// the rail refresh.
    fn set_tiles_locked(&self, state: &mut State, update: impl FnOnce(&mut Tiles)) {
        update(&mut state.tiles);
        self.refresh_pipeline_locked(state);
    }

    /// One stage-tile update: locks the state, applies `update` and
    /// refreshes the rail -- the single update path every tile change goes
    /// through.
    pub(crate) fn update_tiles(&self, update: impl FnOnce(&mut Tiles)) {
        let mut state = self.state();
        self.set_tiles_locked(&mut state, update);
    }

    /// A task's tiles start over: both stages pending (the statuses
    /// reset per task).
    pub(crate) fn task_tiles_started_locked(&self, state: &mut State) {
        self.set_tiles_locked(state, |tiles| tiles.set_all(TileStatus::Pending));
    }

    /// A task ended: its stage tiles are muted again, whatever the
    /// outcome.
    pub(crate) fn task_tiles_finished_locked(&self, state: &mut State) {
        self.set_tiles_locked(state, |tiles| tiles.set_all(TileStatus::Muted));
    }
}
