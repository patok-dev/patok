//! The pipeline-rail model: the source data the
//! engine reports and the shell renders. One tile per enabled stage in order,
//! then the standalone DISCOVER tile.

use serde::{Deserialize, Serialize};

/// One pipeline stage with a rail tile: the four
/// stages the engine runs, research, plan, build and review.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Research,
    Plan,
    Build,
    Review,
}

impl Stage {
    /// The tile's letters on the rail (R, P, B, RV): Review spells its first
    /// two letters because Research already owns the single R.
    pub fn letters(self) -> &'static str {
        match self {
            Self::Research => "R",
            Self::Plan => "P",
            Self::Build => "B",
            Self::Review => "RV",
        }
    }

    /// The stage's full user-facing name, shown in the shell's status bar when
    /// the pointer hovers the stage's tile (T50.1).
    pub fn name(self) -> &'static str {
        match self {
            Self::Research => "Research",
            Self::Plan => "Plan",
            Self::Build => "Build",
            Self::Review => "Review",
        }
    }
}

/// One rail tile's status: muted while the engine is
/// idle, pending for the stages after the active one, active for the running
/// stage, done once a stage ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TileStatus {
    #[default]
    Muted,
    Pending,
    Active,
    Done,
}

/// One rail tile: a stage and its status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tile {
    pub stage: Stage,
    pub status: TileStatus,
}

/// The full pipeline rail: the enabled stage tiles
/// in order, then the standalone tiles.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineState {
    pub stages: Vec<Tile>,
    pub discover: TileStatus,
}

impl PipelineState {
    /// The rail with the stages the engine actually runs today -- research,
    /// plan, build and review -- with every tile muted.
    pub fn today() -> Self {
        Self {
            stages: [Stage::Research, Stage::Plan, Stage::Build, Stage::Review]
                .into_iter()
                .map(|stage| Tile {
                    stage,
                    status: TileStatus::Muted,
                })
                .collect(),
            ..Self::default()
        }
    }

    /// The status of the tile for `stage`, or `None` when the rail does not
    /// list that stage.
    pub fn stage_status(&self, stage: Stage) -> Option<TileStatus> {
        self.stages
            .iter()
            .find(|tile| tile.stage == stage)
            .map(|tile| tile.status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stages_carry_their_rail_letters() {
        assert_eq!(
            [
                Stage::Research.letters(),
                Stage::Plan.letters(),
                Stage::Build.letters(),
                Stage::Review.letters()
            ],
            ["R", "P", "B", "RV"]
        );
    }

    #[test]
    fn the_stages_carry_their_full_names() {
        assert_eq!(
            [
                Stage::Research.name(),
                Stage::Plan.name(),
                Stage::Build.name(),
                Stage::Review.name()
            ],
            ["Research", "Plan", "Build", "Review"]
        );
    }

    #[test]
    fn the_default_rail_is_empty_and_all_muted() {
        let state = PipelineState::default();
        assert!(state.stages.is_empty());
        assert_eq!(state.discover, TileStatus::Muted);
    }

    #[test]
    fn todays_rail_lists_research_plan_build_and_review_muted() {
        let state = PipelineState::today();
        assert_eq!(
            state
                .stages
                .iter()
                .map(|tile| (tile.stage.letters(), tile.status))
                .collect::<Vec<_>>(),
            [
                ("R", TileStatus::Muted),
                ("P", TileStatus::Muted),
                ("B", TileStatus::Muted),
                ("RV", TileStatus::Muted)
            ]
        );
        assert_eq!(state.stage_status(Stage::Research), Some(TileStatus::Muted));
        assert_eq!(state.stage_status(Stage::Plan), Some(TileStatus::Muted));
        assert_eq!(state.stage_status(Stage::Review), Some(TileStatus::Muted));
    }

    #[test]
    fn the_rail_round_trips_through_json() {
        let state = PipelineState {
            stages: vec![
                Tile {
                    stage: Stage::Plan,
                    status: TileStatus::Done,
                },
                Tile {
                    stage: Stage::Build,
                    status: TileStatus::Active,
                },
            ],
            discover: TileStatus::Done,
        };
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<PipelineState>(&json).unwrap(), state);
        assert!(json.contains("\"plan\""));
        // A payload saved before the LEARNINGS and SHIP tiles were removed
        // still deserializes: the leftover keys are ignored.
        assert_eq!(
            serde_json::from_str::<PipelineState>(
                r#"{"stages":[],"ship":"muted","discover":"muted","learnings":"done"}"#
            )
            .unwrap(),
            PipelineState::default()
        );
    }
}
