//! The shell: ratatui UI and gRPC client. Render-only; it holds no pipeline state
//!.

mod app;
pub mod client;
mod headless;
mod markdown;
mod overlay;
mod pipeline;
mod project;
mod run;
mod settings;
mod theme;
mod ui;

pub use app::{
    Action, App, DialogKind, FrameFocus, MenuChoice, QueueRun, SOFT_STOP_PENDING, ThemeModal,
    menu_entries,
};
pub use headless::{HeadlessOutcome, run_until};
pub use overlay::{
    Editor, Entry, FieldKind, Number, Row, SCROLL_MARGIN, SETTINGS_CHOICES, Schema, Section,
    SettingsChoice, SettingsOverlay, StatusLevel, clamp_focus, group_entries, header_fold,
    scroll_offset,
};
pub use pipeline::{
    RAIL_WIDTH, TileId, rail_connector_rects, rail_tile_rects, rail_width, render_rail,
    tile_status, tile_style, tile_text,
};
pub use run::{Outcome, Spawner, restart, run, save_theme, shutdown_progress};
pub use settings::{ShellSettings, to_proto};
pub use theme::{AgentText, Theme, theme_modal_groups};
pub use ui::{
    CLOSE_BUTTON_WIDTH, DIALOG_WATERMARK, INJECT_WATERMARK, SETTINGS_HELP_BESIDE_WIDTH,
    SETTINGS_HELP_PANEL_HEIGHT, SETTINGS_HELP_PANEL_WIDTH, button_text, close_button_rect,
    dialog_area, finished_line, fitted_hints, footer_button_rects, format_elapsed, frame_at,
    frame_constraints, human_duration, menu_area, menu_row_at, output_title, output_title_parts,
    output_title_spans, render, settings_area, settings_body_areas, settings_confirm_area,
    settings_row_at, started_line, stop_area, theme_area, theme_row_at,
};
