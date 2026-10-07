//! The pipeline rail: the one
//! rendering of the engine-reported pipeline state of T49.1. The narrow
//! fixed-width vertical rail sits to the left of the merged view's frames in
//! every engine state, so nothing about it changes when the engine moves
//! between idle and running (the merged view of T30.1 has one face per state,
//! which stands in for the spec's Dashboard/Explore tabs). The renderer and
//! the tile-locating tests share the same rect math, so a tile's rect can
//! never drift from what is drawn.

use patok_core::config::RailMode;
use patok_core::config::SettingValue;
use patok_core::pipeline::{PipelineState, Stage, TileStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::Theme;

/// The rail column's width: narrow and fixed. A fixed `Length` constraint in
/// the layout -- nothing in the app moves a layout edge, so the rail's edge is
/// not draggable by construction.
pub const RAIL_WIDTH: u16 = 6;

/// The blank row between the top of the rail column and the first tile
/// (T63.1): every tile and its connector segments start this many rows below
/// the top of the rail area, so the rail opens with one purely empty line.
/// Nothing is drawn in that row: no border, connector, tile or colour.
const RAIL_TOP_GAP: u16 = 1;

/// One rail tile: the four stage tiles and
/// the standalone tile DI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileId {
    Research,
    Plan,
    Build,
    Review,
    Discover,
}

/// Every tile, stage and standalone, in rail order.
const TILES: [TileId; 5] = [
    TileId::Research,
    TileId::Plan,
    TileId::Build,
    TileId::Review,
    TileId::Discover,
];

impl TileId {
    /// The stage tile's id for `stage`.
    fn of_stage(stage: Stage) -> Self {
        match stage {
            Stage::Research => Self::Research,
            Stage::Plan => Self::Plan,
            Stage::Build => Self::Build,
            Stage::Review => Self::Review,
        }
    }

    /// The stage a stage tile stands for; `None` for the standalone tiles.
    fn stage(self) -> Option<Stage> {
        match self {
            Self::Research => Some(Stage::Research),
            Self::Plan => Some(Stage::Plan),
            Self::Build => Some(Stage::Build),
            Self::Review => Some(Stage::Review),
            _ => None,
        }
    }

    /// The tile's letter or letters: R, P, B, RV for the stages, DI for the standalone tile.
    pub fn letters(self) -> &'static str {
        match self {
            Self::Research => "R",
            Self::Plan => "P",
            Self::Build => "B",
            Self::Review => "RV",
            Self::Discover => "DI",
        }
    }

    /// The tile's full user-facing name, carried by the normal-mode box
    /// (Research, Plan, Build, Review, Discover).
    pub fn label(self) -> &'static str {
        match self {
            Self::Discover => "Discover",
            other => other.stage().map(Stage::name).unwrap_or_default(),
        }
    }
}

/// The rail column's width in `mode` (T57.1): the narrow fixed width for the
/// compact letter tiles, and for normal mode one width wide enough for the
/// longest tile name plus the box padding, so every full-name box in the rail
/// is equally wide.
pub fn rail_width(mode: RailMode) -> u16 {
    match mode {
        RailMode::Compact => RAIL_WIDTH,
        RailMode::Normal | RailMode::Detailed => {
            u16::try_from(longest_label() + 4).unwrap_or(RAIL_WIDTH)
        }
    }
}

/// The longest tile name: the width every normal-mode box centres its name in.
fn longest_label() -> usize {
    TILES
        .iter()
        .map(|id| id.label().chars().count())
        .max()
        .unwrap_or(0)
}

/// The tile as drawn: brackets around the letter(s) in compact mode (the
/// brackets stand in for the spec's tile border, so an accent-bordered tile is
/// one whose brackets carry the accent colour), or in normal mode a bracketed
/// box of the common rail width carrying the tile's full user-facing name,
/// centred (T57.1).
pub fn tile_text(id: TileId, mode: RailMode) -> String {
    match mode {
        RailMode::Compact => format!("[ {} ]", id.letters()),
        RailMode::Normal | RailMode::Detailed => {
            format!("[ {:^width$} ]", id.label(), width = longest_label())
        }
    }
}

/// One tile's status in `state`: the stage's status for a stage tile, the
/// standalone flag for DI.
pub fn tile_status(state: &PipelineState, id: TileId) -> TileStatus {
    match id {
        TileId::Discover => state.discover,
        other => other
            .stage()
            .and_then(|stage| state.stage_status(stage))
            .unwrap_or_default(),
    }
}

/// One tile's style from its status: muted and
/// pending tiles render in the theme's muted colour, done tiles in green, and
/// the active tile is accent-bordered (accent brackets) and bold.
pub fn tile_style(_id: TileId, status: TileStatus, theme: Theme) -> Style {
    let colour = match status {
        TileStatus::Muted | TileStatus::Pending => theme.muted_text,
        TileStatus::Done => theme.success,
        TileStatus::Active => theme.accent,
    };
    if status == TileStatus::Active {
        Style::new().fg(colour).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(colour)
    }
}

/// The rail's tile rects, top to bottom: one blank row (T63.1) between the
/// top of the rail area and the first tile, then the enabled stage tiles in
/// order, a gap and the standalone tiles -- all shifted down by exactly
/// `RAIL_TOP_GAP` rows. Tiles past the area's bottom are dropped, so a short
/// terminal clips the rail's tail.
pub fn rail_tile_rects(state: &PipelineState, area: Rect, mode: RailMode) -> Vec<(TileId, Rect)> {
    let mut rects = Vec::new();
    let mut row = area.y + RAIL_TOP_GAP;
    for (index, tile) in state.stages.iter().enumerate() {
        // One connector row sits below every stage tile except the first.
        let id = TileId::of_stage(tile.stage);
        if index > 0 {
            row += if mode == RailMode::Detailed { 4 } else { 1 };
        }
        if let Some(rect) = rail_tile_rect(id, area, row, mode) {
            rects.push((id, rect));
        }
        row += if mode == RailMode::Detailed { 4 } else { 1 };
    }
    // The blank gap row between the stage tiles and the standalone tiles.
    if !state.stages.is_empty() {
        row += if mode == RailMode::Detailed { 4 } else { 1 };
    }
    let id = TileId::Discover;
    if let Some(rect) = rail_tile_rect(id, area, row, mode) {
        rects.push((id, rect));
    }
    rects
}

/// One rail tile's rect on `row`: the tile centred in the rail's width for the
/// mode, or `None` when the row falls past the area's bottom.
fn rail_tile_rect(id: TileId, area: Rect, row: u16, mode: RailMode) -> Option<Rect> {
    if row >= area.y + area.height {
        return None;
    }
    let width = u16::try_from(tile_text(id, mode).chars().count()).unwrap_or(0);
    let x = area.x + rail_width(mode).saturating_sub(width).div_ceil(2);
    Some(Rect::new(x, row, width, 1))
}

/// The rail's down-arrow connectors: one cell below every stage tile except
/// the last, centred in the rail's width so the arrow lines up under the
/// tile's letter in compact mode and under the centre of the box column in
/// normal mode. The same `RAIL_TOP_GAP` shift (T63.1) keeps each arrow exactly
/// one row below the stage tile above it. Connectors past the area's bottom
/// are dropped.
pub fn rail_connector_rects(state: &PipelineState, area: Rect, mode: RailMode) -> Vec<Rect> {
    (1..state.stages.len() as u16)
        .map(|index| {
            area.y + RAIL_TOP_GAP + index * if mode == RailMode::Detailed { 4 } else { 2 } - 1
        })
        .filter(|row| *row < area.y + area.height)
        .map(|row| Rect::new(area.x + rail_width(mode) / 2, row, 1, 1))
        .collect()
}

/// Draws the vertical rail into its fixed-width
/// column left of the frames, in every engine state (T53.1): one small tile
/// per enabled stage in order, connected by down-arrow lines, then a gap and
/// the standalone tile DI -- the letter tiles of compact
/// mode or the equal-width full-name boxes of normal mode (T57.1), the mode
/// read live from the tui settings so a change relayouts the next frame.
/// The rail opens with one purely blank row above its first tile (T63.1),
/// which the renderer never writes into. Records the rail's rect at the last
/// render.
pub fn render_rail(frame: &mut Frame, app: &App, area: Rect) {
    app.pipeline_area.set(area);
    let theme = app.theme();
    let mode = app.tui.rail_mode;
    let state = &app.pipeline;
    for (id, rect) in rail_tile_rects(state, area, mode) {
        let style = tile_style(id, tile_status(state, id), theme);
        frame.render_widget(
            Paragraph::new(Span::styled(tile_text(id, mode), style)),
            rect,
        );
        if mode == RailMode::Detailed
            && let Some((provider, model)) = agent_details(app, id)
        {
            let x = area.x + 1;
            let width = area.width.saturating_sub(2);
            for (offset, label) in [provider, model].into_iter().enumerate() {
                let detail_rect = Rect::new(x, rect.y.saturating_add(1 + offset as u16), width, 1);
                if detail_rect.y < area.y.saturating_add(area.height) {
                    frame.render_widget(
                        Paragraph::new(label)
                            .style(Style::new().fg(theme.muted_text))
                            .wrap(ratatui::widgets::Wrap { trim: true }),
                        detail_rect,
                    );
                }
            }
        }
    }
    for rect in rail_connector_rects(state, area, mode) {
        frame.render_widget(
            Paragraph::new(Span::styled("↓", Style::new().fg(theme.muted_text))),
            rect,
        );
    }
}

fn agent_details(app: &App, id: TileId) -> Option<(String, String)> {
    let (provider, model_key) = match id {
        TileId::Research => ("provider", "research_model"),
        TileId::Plan => ("provider", "planner_model"),
        TileId::Build => ("provider", "builder_model"),
        TileId::Review => ("provider", "reviewer_model"),
        TileId::Discover => ("provider", "discovery_model"),
    };
    let value = |key: &str| {
        app.settings.get(key).and_then(|v| match v {
            SettingValue::Str(s) if !s.is_empty() => Some(s.clone()),
            _ => None,
        })
    };
    let provider = value(provider)?;
    let model = value(model_key).or_else(|| value("model"))?;
    Some((provider, model))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> PipelineState {
        PipelineState::today()
    }

    /// Today's rail: the four stage tiles in order with one blank row (T63.1)
    /// above the first, a connector between consecutive stage tiles, a gap,
    /// then the standalone tile.
    #[test]
    fn rail_rects_run_top_to_bottom_with_a_connector_row() {
        let area = Rect::new(0, 1, RAIL_WIDTH, 22);
        let rects = rail_tile_rects(&today(), area, RailMode::Compact);
        let rows: Vec<(TileId, u16)> = rects.iter().map(|(id, rect)| (*id, rect.y)).collect();
        assert_eq!(
            rows,
            vec![
                (TileId::Research, 2),
                (TileId::Plan, 4),
                (TileId::Build, 6),
                (TileId::Review, 8),
                (TileId::Discover, 10),
            ]
        );
        // The top row of the rail area holds no tile: the first tile sits
        // exactly one row below it (T63.1).
        assert!(
            !rects.iter().any(|(_, rect)| rect.y == area.y),
            "the row above the first tile is blank"
        );
        assert_eq!(rects.first().unwrap().1.y, area.y + RAIL_TOP_GAP);
        // The tiles stay inside the rail column and never overlap.
        for (_, rect) in &rects {
            assert!(rect.x >= area.x && rect.right() <= area.right());
        }
        // The connectors sit between consecutive stage tiles, under the letter.
        assert_eq!(
            rail_connector_rects(&today(), area, RailMode::Compact),
            vec![
                Rect::new(3, 3, 1, 1),
                Rect::new(3, 5, 1, 1),
                Rect::new(3, 7, 1, 1)
            ]
        );
    }

    /// A short rail keeps the tiles that fit and clips the tail; the blank
    /// top row (T63.1) counts against the tiles that fit.
    #[test]
    fn rail_rects_clip_tiles_past_the_bottom() {
        let rects = rail_tile_rects(&today(), Rect::new(0, 0, RAIL_WIDTH, 3), RailMode::Compact);
        assert_eq!(
            rects.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![TileId::Research]
        );
        assert!(
            rail_connector_rects(&today(), Rect::new(0, 0, RAIL_WIDTH, 1), RailMode::Compact)
                .is_empty()
        );
    }

    /// Normal mode (T57.1): every box is the width of the longest tile name
    /// plus the bracket-and-space padding, so all boxes in the rail are
    /// equally wide, each name is centred in its box, the boxes fill the rail
    /// column, and the connector arrows centre on the box column.
    #[test]
    fn normal_mode_boxes_are_equally_wide_with_centred_names_and_arrows() {
        let mode = RailMode::Normal;
        let longest = longest_label();
        assert_eq!(longest, "Discover".len());
        assert_eq!(rail_width(mode), u16::try_from(longest + 4).unwrap());
        assert_eq!(rail_width(RailMode::Compact), RAIL_WIDTH);

        // Every tile's box carries its full name, centred, all the same width.
        for id in TILES {
            let text = tile_text(id, mode);
            assert_eq!(text.chars().count(), longest + 4, "{id:?}: {text}");
            let name = id.label();
            let left = text.find(name).unwrap_or_else(|| panic!("{id:?}: {text}"));
            let right = text.chars().count() - left - name.chars().count();
            // The name is centred in the common name field: the odd pad cell
            // always falls on the same (right) side, so the padding is
            // consistent across every box.
            assert!(right >= left, "{id:?}: {text}");
            let padding = (longest - name.chars().count()) / 2;
            assert_eq!(left - 2, padding, "{id:?}: {text}");
        }
        assert_eq!(tile_text(TileId::Plan, mode), "[   Plan   ]");
        assert_eq!(tile_text(TileId::Discover, mode), "[ Discover ]");

        // The rects fill the column's width, one per tile, in the same order
        // and rows as compact mode.
        let area = Rect::new(0, 1, rail_width(mode), 22);
        let state = today();
        let rects = rail_tile_rects(&state, area, mode);
        let compact = rail_tile_rects(&state, Rect::new(0, 1, RAIL_WIDTH, 22), RailMode::Compact);
        assert_eq!(
            rects.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            compact.iter().map(|(id, _)| *id).collect::<Vec<_>>()
        );
        assert_eq!(
            rects.iter().map(|(_, r)| r.y).collect::<Vec<_>>(),
            compact.iter().map(|(_, r)| r.y).collect::<Vec<_>>()
        );
        for (_, rect) in &rects {
            assert_eq!(rect.width, rail_width(mode));
            assert_eq!(rect.x, area.x);
            assert_eq!(rect.height, 1);
        }

        // The arrows sit on the box column's centre, between the stage tiles.
        let connectors = rail_connector_rects(&state, area, mode);
        assert_eq!(
            connectors,
            vec![
                Rect::new(rail_width(mode) / 2, 3, 1, 1),
                Rect::new(rail_width(mode) / 2, 5, 1, 1),
                Rect::new(rail_width(mode) / 2, 7, 1, 1)
            ]
        );
        for rect in &connectors {
            assert!(rect.x >= area.x && rect.right() <= area.right());
        }
    }

    /// A short normal-mode rail keeps the tiles that fit and clips the tail,
    /// like compact mode; the blank top row (T63.1) counts against the tiles
    /// that fit.
    #[test]
    fn normal_mode_rects_clip_tiles_past_the_bottom() {
        let mode = RailMode::Normal;
        let width = rail_width(mode);
        let rects = rail_tile_rects(&today(), Rect::new(0, 0, width, 3), mode);
        assert_eq!(
            rects.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![TileId::Research]
        );
        assert!(rail_connector_rects(&today(), Rect::new(0, 0, width, 1), mode).is_empty());
    }

    /// The spec's tile colouring: muted and
    /// pending muted, done green, the active tile accent and bold.
    #[test]
    fn tile_styles_follow_the_spec_colouring() {
        let theme = Theme::DARK;
        for status in [TileStatus::Muted, TileStatus::Pending] {
            let style = tile_style(TileId::Plan, status, theme);
            assert_eq!(style.fg, Some(theme.muted_text));
            assert!(!style.add_modifier.contains(Modifier::BOLD));
        }
        let done = tile_style(TileId::Plan, TileStatus::Done, theme);
        assert_eq!(done.fg, Some(theme.success));
        assert!(!done.add_modifier.contains(Modifier::BOLD));
        let active = tile_style(TileId::Plan, TileStatus::Active, theme);
        assert_eq!(active.fg, Some(theme.accent));
        assert!(active.add_modifier.contains(Modifier::BOLD));
        // DISCOVER is accent while a round runs.
        assert_eq!(
            tile_style(TileId::Discover, TileStatus::Active, theme).fg,
            Some(theme.accent)
        );
    }

    /// The tiles carry their letters and full names.
    #[test]
    fn tiles_carry_their_letters_and_names() {
        assert_eq!(TileId::Plan.letters(), "P");
        assert_eq!(tile_text(TileId::Plan, RailMode::Compact), "[ P ]");
        assert_eq!(
            [
                TileId::Research.label(),
                TileId::Plan.label(),
                TileId::Build.label(),
                TileId::Review.label(),
                TileId::Discover.label(),
            ],
            ["Research", "Plan", "Build", "Review", "Discover"]
        );
    }
}
