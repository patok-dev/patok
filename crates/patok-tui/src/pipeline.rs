//! The pipeline rail: the one rendering of the engine-reported pipeline state.
//! A single horizontal line above the agent output frame, drawn in every
//! engine state, so nothing about it changes when the engine moves between
//! idle and running. It carries the app's running status, the run mode, the
//! pipeline tiles and the session timer, and picks how much of that to show
//! from the terminal width alone -- there is no setting for it. The renderer
//! and the tile-locating tests share the same piece list, so a tile's rect can
//! never drift from what is drawn.

use patok_core::event::Phase;
use patok_core::pipeline::{PipelineState, Stage, TileStatus};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::theme::Theme;
use crate::ui::output::format_elapsed;

/// The rail line's height: always one row.
pub const RAIL_HEIGHT: u16 = 1;

/// The shortest run of rule dashes between the pipeline and the timer.
const MIN_FILL: usize = 3;

/// The timer's reserved width when no session runs, so the rail's detail does
/// not flip between idle and running at the same terminal width.
const TIMER_RESERVE: usize = 5;

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

    /// The tile's letter: R, P, B, R for the stages, D for the standalone tile.
    pub fn letters(self) -> &'static str {
        match self {
            Self::Research | Self::Review => "R",
            Self::Plan => "P",
            Self::Build => "B",
            Self::Discover => "D",
        }
    }

    /// The tile's full user-facing name (Research, Plan, Build, Review, Discover).
    pub fn label(self) -> &'static str {
        match self {
            Self::Discover => "Discover",
            other => other.stage().map(Stage::name).unwrap_or_default(),
        }
    }
}

/// How much the rail shows, from the least to the most; the widest that fits
/// the terminal wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RailDetail {
    /// Status and the pipeline as bare letters, then the timer.
    Minimal,
    /// The run mode joins the status.
    Mode,
    /// The pipeline steps get brackets: `[ R ]`.
    Brackets,
    /// The pipeline steps carry their full names: `[ Research ]`.
    Names,
}

/// What a piece of the rail line is, which decides its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece {
    /// The horizontal rule, dashes and the spaces around the words.
    Rule,
    Status,
    Mode,
    /// The `>` between stage tiles and the gap before the standalone tile.
    Separator,
    Tile(TileId),
    Timer,
}

/// The text the rail line is built from.
#[derive(Clone, Copy)]
struct RailText<'a> {
    status: &'a str,
    mode: &'a str,
    timer: Option<&'a str>,
}

/// The tile as drawn at `detail`.
fn tile_text(id: TileId, detail: RailDetail) -> String {
    match detail {
        RailDetail::Minimal | RailDetail::Mode => id.letters().to_string(),
        RailDetail::Brackets => format!("[ {} ]", id.letters()),
        RailDetail::Names => format!("[ {} ]", id.label()),
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
/// the active tile is accent-coloured and bold.
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

/// The rail line as pieces, left to right, at `detail`: the status, the run
/// mode from `Mode` up, the pipeline, then a rule that absorbs the spare width
/// and the timer. The rule never gets shorter than [`MIN_FILL`]; the pieces
/// are wider than `width` when the terminal is too narrow, and the line then
/// clips at its right edge.
fn rail_pieces(
    state: &PipelineState,
    text: &RailText,
    detail: RailDetail,
    width: usize,
) -> Vec<(Piece, String)> {
    let rule = |s: &str| (Piece::Rule, s.to_string());
    let mut pieces = vec![rule("─ "), (Piece::Status, text.status.to_string())];
    if detail >= RailDetail::Mode {
        pieces.push(rule(" ─ "));
        pieces.push((Piece::Mode, text.mode.to_string()));
    }
    pieces.push(rule(" ─── "));
    for (index, tile) in state.stages.iter().enumerate() {
        if index > 0 {
            pieces.push((Piece::Separator, " > ".to_string()));
        }
        let id = TileId::of_stage(tile.stage);
        pieces.push((Piece::Tile(id), tile_text(id, detail)));
    }
    if !state.stages.is_empty() {
        pieces.push((Piece::Separator, "   ".to_string()));
    }
    pieces.push((
        Piece::Tile(TileId::Discover),
        tile_text(TileId::Discover, detail),
    ));

    let head: usize = pieces.iter().map(|(_, s)| s.chars().count()).sum();
    match text.timer {
        Some(timer) => {
            // " ", the rule, " ", the timer, " ─".
            let timer_width = timer.chars().count();
            let fill = width.saturating_sub(head + timer_width + 4).max(MIN_FILL);
            pieces.push(rule(&format!(" {} ", "─".repeat(fill))));
            pieces.push((Piece::Timer, timer.to_string()));
            pieces.push(rule(" ─"));
        }
        None => {
            // No session: the rule runs on to the edge.
            let fill = width.saturating_sub(head + 1).max(MIN_FILL);
            pieces.push(rule(&format!(" {}", "─".repeat(fill))));
        }
    }
    pieces
}

/// The widest detail whose natural width fits `width`; `Minimal` when even
/// that does not.
fn pick_detail(state: &PipelineState, text: &RailText, width: usize) -> RailDetail {
    // The decision reserves the timer's room, shown or not.
    let reserved = text.timer.unwrap_or("00:00");
    let probe = RailText {
        timer: Some(reserved),
        ..*text
    };
    [RailDetail::Names, RailDetail::Brackets, RailDetail::Mode]
        .into_iter()
        .find(|detail| {
            let natural: usize = rail_pieces(state, &probe, *detail, 0)
                .iter()
                .map(|(_, s)| s.chars().count())
                .sum();
            natural.max(TIMER_RESERVE) <= width
        })
        .unwrap_or(RailDetail::Minimal)
}

/// The status word and its colour: a discovery round runs either on an idle
/// engine or inside a session's empty-queue gap, so it wins over the phase.
fn rail_status(app: &App) -> (&'static str, Color) {
    let theme = app.theme();
    match (app.run.stopping, app.run.phase) {
        (true, _) => ("Stopping", theme.highlighted_text),
        (false, _) if app.run.planning => ("Planning", theme.chip_planning),
        (false, _) if app.run.discovering => ("Discovering", theme.chip_discovering),
        (false, Phase::Running) => ("Running", theme.success),
        (false, Phase::Startup) => ("Stopped", theme.chip_neutral),
    }
}

/// The rail's pieces for `app` at `width`, with the detail picked from it.
fn app_pieces(app: &App, width: u16) -> (RailDetail, Vec<(Piece, String)>) {
    let (status, _) = rail_status(app);
    let timer = app.session_elapsed().map(format_elapsed);
    let text = RailText {
        status,
        mode: app.run_mode(),
        timer: timer.as_deref(),
    };
    let width = usize::from(width);
    let detail = pick_detail(&app.pipeline, &text, width);
    (detail, rail_pieces(&app.pipeline, &text, detail, width))
}

/// The detail the rail shows at `width` for `app`.
#[cfg(any(test, feature = "test-support"))]
pub fn rail_detail(app: &App, width: u16) -> RailDetail {
    app_pieces(app, width).0
}

/// The tiles' rects on the rail line inside `area`, left to right, clipped to
/// the area: a tile the terminal cuts off is dropped. The same pieces the
/// renderer draws, so the rects are exactly where the tiles are.
#[cfg(any(test, feature = "test-support"))]
pub fn rail_tile_rects(app: &App, area: Rect) -> Vec<(TileId, Rect)> {
    let mut rects = Vec::new();
    let mut x = 0usize;
    for (piece, text) in app_pieces(app, area.width).1 {
        let width = text.chars().count();
        if let Piece::Tile(id) = piece
            && x + width <= usize::from(area.width)
        {
            let x = area.x + u16::try_from(x).unwrap_or(0);
            rects.push((
                id,
                Rect::new(x, area.y, u16::try_from(width).unwrap_or(0), 1),
            ));
        }
        x += width;
    }
    rects
}

/// Draws the rail line into `area`, one row, in every engine state. Records
/// the rail's rect at the last render.
pub fn render_rail(frame: &mut Frame, app: &App, area: Rect) {
    app.layout.pipeline_area.set(area);
    let theme = app.theme();
    let (_, status_colour) = rail_status(app);
    let muted = Style::new().fg(theme.muted_text);
    let spans: Vec<Span> = app_pieces(app, area.width)
        .1
        .into_iter()
        .map(|(piece, text)| {
            let style = match piece {
                Piece::Rule => Style::new(),
                Piece::Status => Style::new().fg(status_colour).add_modifier(Modifier::BOLD),
                Piece::Mode | Piece::Separator | Piece::Timer => muted,
                Piece::Tile(id) => tile_style(id, tile_status(&app.pipeline, id), theme),
            };
            Span::styled(text, style)
        })
        .collect();
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> PipelineState {
        PipelineState::today()
    }

    fn line(text: &RailText, detail: RailDetail, width: usize) -> String {
        rail_pieces(&today(), text, detail, width)
            .into_iter()
            .map(|(_, s)| s)
            .collect()
    }

    const CONTINUOUS: RailText = RailText {
        status: "Running",
        mode: "continuous",
        timer: Some("00:00"),
    };
    const SPRINT: RailText = RailText {
        status: "Running",
        mode: "sprint",
        timer: Some("00:00"),
    };

    /// Each detail draws the line the design shows, at its natural width.
    #[test]
    fn each_detail_draws_its_line() {
        assert_eq!(
            line(&CONTINUOUS, RailDetail::Minimal, 0),
            "─ Running ─── R > P > B > R   D ─── 00:00 ─"
        );
        assert_eq!(
            line(&CONTINUOUS, RailDetail::Mode, 0),
            "─ Running ─ continuous ─── R > P > B > R   D ─── 00:00 ─"
        );
        assert_eq!(
            line(&CONTINUOUS, RailDetail::Brackets, 0),
            "─ Running ─ continuous ─── [ R ] > [ P ] > [ B ] > [ R ]   [ D ] ─── 00:00 ─"
        );
        assert_eq!(
            line(&CONTINUOUS, RailDetail::Names, 0),
            "─ Running ─ continuous ─── [ Research ] > [ Plan ] > [ Build ] > [ Review ]   [ Discover ] ─── 00:00 ─"
        );
    }

    /// A shorter mode word leaves the spare width to the rule.
    #[test]
    fn the_rule_absorbs_the_spare_width() {
        let width = line(&CONTINUOUS, RailDetail::Brackets, 0).chars().count();
        assert_eq!(
            line(&SPRINT, RailDetail::Brackets, width),
            "─ Running ─ sprint ─── [ R ] > [ P ] > [ B ] > [ R ]   [ D ] ─────── 00:00 ─"
        );
        assert_eq!(
            line(&SPRINT, RailDetail::Brackets, width).chars().count(),
            width
        );
    }

    /// With no session the rule runs to the edge instead of the timer.
    #[test]
    fn an_idle_rail_has_no_timer() {
        let idle = RailText {
            timer: None,
            ..CONTINUOUS
        };
        let text = line(&idle, RailDetail::Minimal, 50);
        assert_eq!(
            text,
            format!("─ Running ─── R > P > B > R   D {}", "─".repeat(18))
        );
    }

    /// The detail follows the width alone: each step up appears exactly at the
    /// previous natural width of the richer line.
    #[test]
    fn the_detail_grows_with_the_width() {
        let state = today();
        let natural = |detail| line(&CONTINUOUS, detail, 0).chars().count();
        for (detail, width) in [
            (RailDetail::Mode, natural(RailDetail::Mode)),
            (RailDetail::Brackets, natural(RailDetail::Brackets)),
            (RailDetail::Names, natural(RailDetail::Names)),
        ] {
            assert_eq!(pick_detail(&state, &CONTINUOUS, width), detail);
            assert!(pick_detail(&state, &CONTINUOUS, width - 1) < detail);
        }
        assert_eq!(pick_detail(&state, &CONTINUOUS, 10), RailDetail::Minimal);
        assert_eq!(pick_detail(&state, &CONTINUOUS, 500), RailDetail::Names);
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
        assert_eq!(TileId::Review.letters(), "R");
        assert_eq!(TileId::Discover.letters(), "D");
        assert_eq!(tile_text(TileId::Plan, RailDetail::Brackets), "[ P ]");
        assert_eq!(tile_text(TileId::Plan, RailDetail::Names), "[ Plan ]");
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
