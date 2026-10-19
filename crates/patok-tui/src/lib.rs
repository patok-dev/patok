//! The shell: ratatui UI. Render-only; it holds no pipeline state
//!.

mod app;
mod choice;
mod dialog_editor;
mod driver;
mod events;
mod geom;
mod headless;
mod hit;
mod keys;
mod markdown;
mod modal;
mod mouse;
mod overlay;
mod pipeline;
mod project;
mod run;
mod settings;
mod status;
mod submit;
mod theme;
mod theme_picker;
mod ui;
mod view;

// The surface the `patok` binary drives: the two drivers' entries and
// their shared outcome types, plus the per-daemon status client.
pub use headless::{HeadlessOutcome, run_until};
pub use run::{Outcome, Spawner, run};
pub use status::{DaemonStatus, RunState, StatusStream, StatusUpdate, TaskProgress, probe};

// The internals the crate's own integration tests reach: re-exported only
// under the test-support feature, enabled for those tests through the
// self dev-dependency in Cargo.toml.
#[cfg(any(test, feature = "test-support"))]
pub use app::{Action, App, DialogKind, QueueRun, SOFT_STOP_PENDING, menu_entries};
#[cfg(any(test, feature = "test-support"))]
pub use hit::{
    CLOSE_BUTTON_WIDTH, button_text, close_button_rect, footer_button_rects, frame_at, theme_row_at,
};
#[cfg(any(test, feature = "test-support"))]
pub use modal::Modal;
#[cfg(any(test, feature = "test-support"))]
pub use overlay::{Entry, FieldKind, SETTINGS_CHOICES, Schema, StatusLevel};
// The line formatters and the duration helper, re-exported from their home
// in patok-core for the test surface.
#[cfg(any(test, feature = "test-support"))]
pub use patok_core::output::{finished_line, human_duration, started_line};
#[cfg(any(test, feature = "test-support"))]
pub use pipeline::{RailDetail, TileId, rail_detail, rail_tile_rects};
#[cfg(any(test, feature = "test-support"))]
pub use run::{restart, save_theme, shutdown_progress};
#[cfg(any(test, feature = "test-support"))]
pub use settings::{ShellSettings, to_proto};
#[cfg(any(test, feature = "test-support"))]
pub use theme::{Theme, theme_modal_groups};
#[cfg(any(test, feature = "test-support"))]
pub use ui::{
    DIALOG_WATERMARK, SETTINGS_HELP_BESIDE_WIDTH, SETTINGS_HELP_PANEL_HEIGHT,
    SETTINGS_HELP_PANEL_WIDTH, collapses_to_line, dialog_area, fitted_hints, format_elapsed,
    frame_constraints, output_title, output_title_parts, output_title_spans, render, settings_area,
    settings_body_areas, settings_confirm_area, stop_area, theme_area,
};
#[cfg(any(test, feature = "test-support"))]
pub use view::FrameFocus;
