//! The colour theme (T34.1): the single source of every colour the shell renders.
//!
//! Every semantic colour the renderers use lives here as a named field; no code
//! outside this module constructs a `ratatui::style::Color` literal. Renderers
//! reach the theme through [`crate::app::App::theme`], which resolves it from the
//! shell's tui settings, so a config reload picks up changes without extra
//! plumbing. Markdown rendering takes its colours from the base style
//! (`thinking`), so it needs no fields of its own.
//!
//! Eleven built-in variants: the default `dark` plus ten published palettes
//! (Atom One, Tokyo Night, Catppuccin, Solarized and Gruvbox, each in a dark and
//! a light variant). Every non-default variant is defined once as a
//! [`Palette`] of signature roles and mapped onto the same semantic fields by
//! the same rules, so every renderer works unchanged with any variant. RGB
//! colours emit as `Color::Rgb` while the truecolor setting is on and as the
//! nearest xterm-256 indexed colour while it is off.
//!
//! The agent output line colours live in their own [`AgentText`] subclass
//! (T108.1), structurally separate from the chrome fields so the output
//! identity stays stable across theme refactoring. Each line kind's colour
//! identity is fixed for every built-in theme — error red, notice yellow, tool
//! cyan, thinking blue, heading and pane status magenta, result a muted gray —
//! and a palette only adjusts an anchor's lightness so the fixed identity
//! stays readable on that theme's background instead of remapping the kinds
//! onto palette roles. Both the frame's `style_of` and the headless mode's
//! `--color` output read the subclass through [`Theme::line_color`], so the
//! two cannot drift.
//!
//! Field-to-use map (all sites are in `ui.rs` unless noted):
//!
//! | Field | Renderer | Use |
//! |---|---|---|
//! | `background` | `render` | the frame's base background, painted first |
//! | `foreground` | `render` | the frame's base foreground, inherited by unstyled text; the modal buttons' labels (T64.1) |
//! | `chip_stopped` | `status_widget` | STOPPED status chip background |
//! | `chip_running` | `status_widget` | RUNNING status chip background |
//! | `chip_planning` | `status_widget` | PLANNING status chip background |
//! | `chip_discovering` | `status_widget` | DISCOVERING status chip background |
//! | `chip_stopping` | `status_widget` | STOPPING status chip background |
//! | `chip_text` | `status_widget` | foreground on every status chip |
//! | `run_mode_chip` | `status_widget` | run-mode chip background |
//! | `status_message` | `status_widget` | the status bar's one-line message |
//! | `status_key` | `status_widget` | key-chip background (e.g. " Enter ") |
//! | `status_label` | `status_widget` | key-chip labels (e.g. "run discovery") |
//! | `agent_text.thinking` | `style_of`, headless `--color` | agent thinking; also the markdown base style |
//! | `agent_text.tool` | `style_of`, headless `--color` | tool-call lines |
//! | `agent_text.result` | `style_of`, headless `--color` | result lines |
//! | `agent_text.error` | `style_of`, headless `--color` | error lines |
//! | `agent_text.notice` | `style_of`, headless `--color` | notice lines |
//! | `agent_text.heading` | `style_of`, headless `--color` | markdown headings (bold added by the renderer) |
//! | `frame_title_builder` | `render_output` | the agent type name in the output frame title while the builder runs |
//! | `frame_title_planner` | `render_output` | the agent type name in the output frame title while the planner runs |
//! | `frame_title_research` | `render_output` | the agent type name in the output frame title while the research agent runs (T68.1) |
//! | `frame_title_detail` | `render_output`, `render_tasks` | output frame title's separator, provider, model and timer; everything after the word `Tasks` in the tasks frame title (T67.1): the pipe separator, the completed, total and left counts, the slash, the dash and the word `left` |
//! | `agent_text.pane_status` | `style_of`, headless `--color` | the agent lifecycle status lines in the output pane: the started line (T78.1) and the finished line (T42.1), in the same colour as the heading/task lines (T96.1) |
//! | `pane_empty` | `render_pane` | "no output yet" placeholder |
//! | `task_done` | `render_tasks` | done task rows |
//! | `task_running` | `render_tasks` | running task rows (bold added by the renderer) |
//! | `task_new` | `render_tasks` | new-task highlight (bold + reversed added by the renderer) |
//! | `tasks_empty` | `render_tasks` | "no tasks in TASKS.md" placeholder |
//! | `dialog_status` | `render_dialog` | the add-task dialog's status line |
//! | `cursor_foreground` | `render_dialog` | the dialog input's block cursor foreground |
//! | `cursor_background` | `render_dialog` | the dialog input's block cursor background |
//! | `dialog_hint` | `render_dialog` | the empty-input watermark (T41.1) |
//! | `choice_detail` | `render_stop_dialog`, `render_settings_confirm` | "-- detail" choice text |
//! | `modal_footer` | `render_modal_footer`, `render_hints_strip` | every modal bottom line's key-hint zone (T59.1) and the focused dashboard frame's hints strip along its bottom edge (T71.1) |
//! | `button_accent` | `render_modal_footer`, `close_button_line` | every modal's bracketed " [ Key ] " key markers and the " [ x ] " close button of every modal's title row (T44.1, T59.1, T66.1); the labels after them wear `foreground` (T64.1) |
//! | `settings_error` | `render_settings_overlay` | overlay error status line |
//! | `settings_info` | `render_settings_overlay` | overlay info status line |
//! | `settings_readonly` | `settings_row_line` | read-only overlay rows |
//! | `settings_help` | `render_settings_overlay` | the settings help box text (T85.1) |
//! | `scrollbar_thumb` | `render_scrollbar` | overlay scrollbar thumb |
//! | `scrollbar_rail` | `render_scrollbar` | overlay scrollbar rail |
//! | `rail_done` | `pipeline::tile_style` | done rail tiles (T50.1): stages before the active one, SHIP while shipping, DISCOVER after a round ran, LEARNINGS once learned |
//! | `rail_active` | `pipeline::tile_style` | the active stage tile and DISCOVER while a round runs (bold added by the renderer) |
//! | `rail_muted` | `pipeline::tile_style` | muted and pending rail tiles |
//! | `rail_connector` | `pipeline::render_rail` | the rail's down-arrow connectors |

use crate::app::LineKind;
use patok_core::config::Theme as ThemeKey;
use ratatui::style::Color;

/// The agent output line colours (T108.1): structurally separate from the
/// chrome fields so the output identity stays stable across theme
/// refactoring. Each kind's hue is fixed for every built-in theme; a palette
/// only adjusts lightness so the fixed hue stays readable on its background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentText {
    /// Agent thinking lines; the markdown base style.
    pub thinking: Color,
    /// Tool-call lines.
    pub tool: Color,
    /// Result lines: a muted gray, independent of the chrome `muted` role.
    pub result: Color,
    /// Error lines.
    pub error: Color,
    /// Notice lines.
    pub notice: Color,
    /// Agent lifecycle status lines in the output pane (heading colour).
    pub pane_status: Color,
    /// Markdown headings (the renderer adds bold).
    pub heading: Color,
}

/// The canonical anchor colours of the agent line kinds (T108.1): one RGB
/// triple per kind, fixed for every built-in theme so the kind's colour
/// identity never changes. A palette only adjusts an anchor's lightness
/// against its background ([`adjust_lightness`]); hue and saturation are
/// preserved exactly.
const THINKING_ANCHOR: (u8, u8, u8) = (0x5C, 0x8D, 0xEB);
/// Tool lines: cyan.
const TOOL_ANCHOR: (u8, u8, u8) = (0x4F, 0xC1, 0xC9);
/// Result lines: a neutral gray.
const RESULT_ANCHOR: (u8, u8, u8) = (0x98, 0x98, 0x98);
/// Error lines: red.
const ERROR_ANCHOR: (u8, u8, u8) = (0xE0, 0x52, 0x52);
/// Notice lines: yellow.
const NOTICE_ANCHOR: (u8, u8, u8) = (0xD9, 0xB0, 0x2B);
/// Heading and pane-status lines: magenta.
const HEADING_ANCHOR: (u8, u8, u8) = (0xB7, 0x6B, 0xD8);

/// The WCAG contrast ratio a palette's lightness adjustment (T108.1) targets:
/// the least an agent line colour must reach against its theme's background.
const READABLE_CONTRAST: f64 = 3.0;
/// How far [`adjust_lightness`] steps the lightness channel per iteration, and
/// the bounds it never leaves, so the shift stays small.
const LIGHTNESS_STEP: f64 = 0.02;
const LIGHTNESS_MIN: f64 = 0.15;
const LIGHTNESS_MAX: f64 = 0.9;

impl AgentText {
    /// Resolves the agent line colours for a palette theme (T108.1): every
    /// kind keeps its canonical anchor; the theme's background only adjusts
    /// each anchor's lightness until the pair reads. The truecolor setting
    /// picks the emission, exactly like the chrome roles.
    fn resolve(background: (u8, u8, u8), truecolor: bool) -> AgentText {
        let colour = |rgb: (u8, u8, u8)| {
            if truecolor {
                Color::Rgb(rgb.0, rgb.1, rgb.2)
            } else {
                Color::Indexed(rgb_to_indexed(rgb))
            }
        };
        let line = |anchor: (u8, u8, u8)| colour(adjust_lightness(anchor, background));
        AgentText {
            thinking: line(THINKING_ANCHOR),
            tool: line(TOOL_ANCHOR),
            result: line(RESULT_ANCHOR),
            error: line(ERROR_ANCHOR),
            notice: line(NOTICE_ANCHOR),
            pane_status: line(HEADING_ANCHOR),
            heading: line(HEADING_ANCHOR),
        }
    }
}

/// Converts an RGB triple to HSL: hue in degrees (0..360), saturation and
/// lightness in 0..1. Used only by the lightness adjustment and the identity
/// tests, so it needs no colour-space refinement. Near-equal channels are
/// achromatic: hue and saturation collapse to zero.
fn rgb_to_hsl(rgb: (u8, u8, u8)) -> (f64, f64, f64) {
    let r = f64::from(rgb.0) / 255.0;
    let g = f64::from(rgb.1) / 255.0;
    let b = f64::from(rgb.2) / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    let delta = max - min;
    if delta < 1.0 / 255.0 {
        return (0.0, 0.0, lightness);
    }
    let saturation = if lightness > 0.5 {
        delta / (2.0 - max - min)
    } else {
        delta / (max + min)
    };
    let hue = if max == r {
        (g - b) / delta + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    (hue * 60.0, saturation, lightness)
}

/// Converts HSL back to RGB: the inverse of [`rgb_to_hsl`], with each channel
/// rounded to the nearest byte.
fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> (u8, u8, u8) {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue / 60.0;
    let secondary = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (r, g, b) = if sector < 1.0 {
        (chroma, secondary, 0.0)
    } else if sector < 2.0 {
        (secondary, chroma, 0.0)
    } else if sector < 3.0 {
        (0.0, chroma, secondary)
    } else if sector < 4.0 {
        (0.0, secondary, chroma)
    } else if sector < 5.0 {
        (secondary, 0.0, chroma)
    } else {
        (chroma, 0.0, secondary)
    };
    let match_lightness = lightness - chroma / 2.0;
    let channel = |value: f64| (255.0 * (value + match_lightness).clamp(0.0, 1.0)).round() as u8;
    (channel(r), channel(g), channel(b))
}

/// The WCAG relative luminance of an RGB triple.
fn luminance(rgb: (u8, u8, u8)) -> f64 {
    let channel = |value: u8| {
        let c = f64::from(value) / 255.0;
        if c <= 0.039_28 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(rgb.0) + 0.7152 * channel(rgb.1) + 0.0722 * channel(rgb.2)
}

/// The WCAG contrast ratio of two RGB triples: at least 1.0, at most 21.0.
fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let (high, low) = if luminance(a) > luminance(b) {
        (luminance(a), luminance(b))
    } else {
        (luminance(b), luminance(a))
    };
    (high + 0.05) / (low + 0.05)
}

/// The palette's only allowed adjustment (T108.1): keep the anchor's hue and
/// saturation, step its lightness away from the background's until the pair
/// reaches [`READABLE_CONTRAST`], bounded so the shift stays small. An anchor
/// that already reads on the background (typical on dark backgrounds) comes
/// back unchanged.
fn adjust_lightness(rgb: (u8, u8, u8), background: (u8, u8, u8)) -> (u8, u8, u8) {
    if contrast_ratio(rgb, background) >= READABLE_CONTRAST {
        return rgb;
    }
    let (hue, saturation, lightness) = rgb_to_hsl(rgb);
    // Step up on dark backgrounds, down on light ones: away from wherever the
    // background sits.
    let darken = luminance(background) >= luminance(rgb);
    let steps = ((LIGHTNESS_MAX - LIGHTNESS_MIN) / LIGHTNESS_STEP) as usize;
    let mut best = rgb;
    for step in 1..=steps {
        let shifted = LIGHTNESS_STEP * step as f64;
        let candidate_lightness = if darken {
            (lightness - shifted).max(LIGHTNESS_MIN)
        } else {
            (lightness + shifted).min(LIGHTNESS_MAX)
        };
        let candidate = hsl_to_rgb(hue, saturation, candidate_lightness);
        best = candidate;
        if contrast_ratio(candidate, background) >= READABLE_CONTRAST {
            return candidate;
        }
    }
    best
}

/// The shell's colour theme: one named semantic field per colour use, so a future
/// variant can restyle any use independently. `Copy`, so renderers pass it around
/// freely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// The shell's base background, painted over the whole frame first.
    pub background: Color,
    /// The shell's base foreground, inherited by every unstyled text cell.
    pub foreground: Color,
    /// Status-line STOPPED status chip background.
    pub chip_stopped: Color,
    /// Status-line RUNNING status chip background.
    pub chip_running: Color,
    /// Status-line PLANNING status chip background.
    pub chip_planning: Color,
    /// Status-line DISCOVERING status chip background.
    pub chip_discovering: Color,
    /// Status-line STOPPING status chip background.
    pub chip_stopping: Color,
    /// Foreground on every status-line status chip.
    pub chip_text: Color,
    /// Run-mode chip background (foreground `chip_text`).
    pub run_mode_chip: Color,
    /// The status bar's one-line message.
    pub status_message: Color,
    /// Status-bar key-chip background (foreground `chip_text`).
    pub status_key: Color,
    /// Status-bar key-chip labels.
    pub status_label: Color,
    /// The agent output line colours (T108.1), held apart from the chrome
    /// fields so the output identity stays stable across theme refactoring.
    pub agent_text: AgentText,
    /// The agent type name in the output frame title while the builder runs.
    pub frame_title_builder: Color,
    /// The agent type name in the output frame title while the planner runs.
    pub frame_title_planner: Color,
    /// The agent type name in the output frame title while the research agent
    /// runs (T68.1).
    pub frame_title_research: Color,
    /// The output frame title's separator, provider, model and timer text (T37.1),
    /// and everything after the word `Tasks` in the tasks frame title (T67.1):
    /// the pipe separator, the completed, total and left counts, the slash, the
    /// dash and the word `left`.
    pub frame_title_detail: Color,
    /// "no output yet" placeholder in the output pane.
    pub pane_empty: Color,
    /// Done task rows.
    pub task_done: Color,
    /// Running task rows (the renderer adds bold).
    pub task_running: Color,
    /// New-task highlight (the renderer adds bold + reversed).
    pub task_new: Color,
    /// "no tasks in TASKS.md" placeholder.
    pub tasks_empty: Color,
    /// The add-task dialog's status line.
    pub dialog_status: Color,
    /// The dialog input's block cursor foreground.
    pub cursor_foreground: Color,
    /// The dialog input's block cursor background.
    pub cursor_background: Color,
    /// The empty-input watermark (T41.1).
    pub dialog_hint: Color,
    /// "-- detail" text in the stop and unsaved-changes dialogs.
    pub choice_detail: Color,
    /// Every modal bottom line's key-hint zone (T59.1).
    pub modal_footer: Color,
    /// Every modal bottom line's " [ Key ] Label " buttons and the settings
    /// overlay's " [ X ] " close marker (T44.1, T59.1).
    pub button_accent: Color,
    /// The settings overlay's error status line.
    pub settings_error: Color,
    /// The settings overlay's info status line.
    pub settings_info: Color,
    /// Read-only settings overlay rows.
    pub settings_readonly: Color,
    /// The settings overlay's help box text (T85.1).
    pub settings_help: Color,
    /// The settings overlay's scrollbar thumb.
    pub scrollbar_thumb: Color,
    /// The settings overlay's scrollbar rail.
    pub scrollbar_rail: Color,
    /// Done rail tiles (T50.1): the stages before the active one,
    /// SHIP while shipping, DISCOVER after a round ran, LEARNINGS once learned.
    pub rail_done: Color,
    /// The active stage tile and DISCOVER while a round runs (bold added).
    pub rail_active: Color,
    /// Muted and pending rail tiles.
    pub rail_muted: Color,
    /// The rail's down-arrow connectors between consecutive stage tiles.
    pub rail_connector: Color,
}

impl Theme {
    /// The default dark theme: exactly the look the shell had before themes
    /// existed, value-locked by the unit tests below.
    pub const DARK: Theme = Theme {
        background: Color::Reset,
        foreground: Color::Reset,
        chip_stopped: Color::DarkGray,
        chip_running: Color::Green,
        chip_planning: Color::Magenta,
        chip_discovering: Color::Blue,
        chip_stopping: Color::Yellow,
        chip_text: Color::Black,
        run_mode_chip: Color::DarkGray,
        status_message: Color::Yellow,
        status_key: Color::DarkGray,
        status_label: Color::DarkGray,
        agent_text: AgentText {
            thinking: Color::Blue,
            tool: Color::Cyan,
            result: Color::DarkGray,
            error: Color::Red,
            notice: Color::Yellow,
            pane_status: Color::Magenta,
            heading: Color::Magenta,
        },
        frame_title_builder: Color::Green,
        frame_title_planner: Color::Magenta,
        frame_title_research: Color::LightBlue,
        frame_title_detail: Color::DarkGray,
        pane_empty: Color::DarkGray,
        task_done: Color::DarkGray,
        task_running: Color::Green,
        task_new: Color::Yellow,
        tasks_empty: Color::DarkGray,
        dialog_status: Color::Yellow,
        cursor_foreground: Color::Black,
        cursor_background: Color::Cyan,
        dialog_hint: Color::DarkGray,
        choice_detail: Color::DarkGray,
        modal_footer: Color::DarkGray,
        button_accent: Color::Yellow,
        settings_error: Color::Yellow,
        settings_info: Color::DarkGray,
        settings_readonly: Color::DarkGray,
        settings_help: Color::DarkGray,
        scrollbar_thumb: Color::Cyan,
        scrollbar_rail: Color::DarkGray,
        rail_done: Color::Green,
        rail_active: Color::Cyan,
        rail_muted: Color::DarkGray,
        rail_connector: Color::DarkGray,
    };

    /// Resolves the theme from the shell's tui settings. `dark` is the
    /// value-locked default; every other key maps to its built-in palette. The
    /// truecolor setting picks the emission: `Some(true)` forces RGB,
    /// `Some(false)` forces the nearest xterm-256 indexed colour, and `None`
    /// (the "auto" state) detects the terminal's `COLORTERM`. `Theme::DARK`
    /// uses named ANSI colours and is unaffected by the setting.
    pub fn resolve(theme: ThemeKey, truecolor: Option<bool>) -> Theme {
        match theme {
            ThemeKey::Dark => Theme::DARK,
            other => palette_of(other).theme(truecolor.unwrap_or_else(truecolor_detected)),
        }
    }

    /// The foreground colour of one output line kind: the single kind-to-colour
    /// mapping, whose identity lives on the [`AgentText`] subclass (T108.1),
    /// shared by the frame's `style_of` and the headless mode's `--color`
    /// output so the two cannot drift. `Text` lines render unstyled (the
    /// caller decides what that means), so it maps to `Color::Reset`.
    pub fn line_color(theme: Theme, kind: LineKind) -> Color {
        match kind {
            LineKind::Text => Color::Reset,
            LineKind::Thinking => theme.agent_text.thinking,
            LineKind::Tool => theme.agent_text.tool,
            LineKind::Result => theme.agent_text.result,
            LineKind::Error => theme.agent_text.error,
            LineKind::Notice => theme.agent_text.notice,
            LineKind::Status => theme.agent_text.pane_status,
            LineKind::Heading => theme.agent_text.heading,
        }
    }
}

/// One published palette's signature roles, as raw RGB triples. The roles map
/// onto the semantic fields by one shared rule ([`Palette::theme`]), which is
/// what keeps every renderer variant-agnostic.
struct Palette {
    /// The theme's base background.
    background: (u8, u8, u8),
    /// The theme's base foreground.
    foreground: (u8, u8, u8),
    /// Comments and dimmed text.
    muted: (u8, u8, u8),
    /// Muted backgrounds: selections and quiet chips.
    surface: (u8, u8, u8),
    green: (u8, u8, u8),
    /// The palette's warning colour.
    yellow: (u8, u8, u8),
    /// The palette's accent.
    blue: (u8, u8, u8),
    magenta: (u8, u8, u8),
}

const ATOM_ONE_DARK: Palette = Palette {
    background: (0x28, 0x2C, 0x34),
    foreground: (0xAB, 0xB2, 0xBF),
    muted: (0x5C, 0x63, 0x70),
    surface: (0x3E, 0x44, 0x51),
    green: (0x98, 0xC3, 0x79),
    yellow: (0xE5, 0xC0, 0x7B),
    blue: (0x61, 0xAF, 0xEF),
    magenta: (0xC6, 0x78, 0xDD),
};

const ATOM_ONE_LIGHT: Palette = Palette {
    background: (0xFA, 0xFA, 0xFA),
    foreground: (0x38, 0x3A, 0x42),
    muted: (0xA0, 0xA1, 0xA7),
    surface: (0xE5, 0xE5, 0xE5),
    green: (0x50, 0xA1, 0x4F),
    yellow: (0xC1, 0x84, 0x01),
    blue: (0x40, 0x78, 0xF2),
    magenta: (0xA6, 0x26, 0xA4),
};

const TOKYO_NIGHT_DARK: Palette = Palette {
    background: (0x1A, 0x1B, 0x26),
    foreground: (0xC0, 0xCA, 0xF5),
    muted: (0x56, 0x5F, 0x89),
    surface: (0x33, 0x46, 0x7C),
    green: (0x9E, 0xCE, 0x6A),
    yellow: (0xE0, 0xAF, 0x68),
    blue: (0x7A, 0xA2, 0xF7),
    magenta: (0xBB, 0x9A, 0xF7),
};

const TOKYO_NIGHT_DAY: Palette = Palette {
    background: (0xE1, 0xE2, 0xE7),
    foreground: (0x37, 0x60, 0xBF),
    muted: (0x84, 0x8C, 0xB5),
    surface: (0xC4, 0xC8, 0xDA),
    green: (0x58, 0x75, 0x39),
    yellow: (0x8C, 0x6C, 0x3E),
    blue: (0x2E, 0x7D, 0xE9),
    magenta: (0x98, 0x54, 0xF1),
};

const CATPPUCCIN_MOCHA: Palette = Palette {
    background: (0x1E, 0x1E, 0x2E),
    foreground: (0xCD, 0xD6, 0xF4),
    muted: (0x7F, 0x84, 0x9C),
    surface: (0x31, 0x32, 0x44),
    green: (0xA6, 0xE3, 0xA1),
    yellow: (0xF9, 0xE2, 0xAF),
    blue: (0x89, 0xB4, 0xFA),
    magenta: (0xCB, 0xA6, 0xF7),
};

const CATPPUCCIN_LATTE: Palette = Palette {
    background: (0xEF, 0xF1, 0xF5),
    foreground: (0x4C, 0x4F, 0x69),
    muted: (0x7C, 0x7F, 0x94),
    surface: (0xCC, 0xD0, 0xDA),
    green: (0x40, 0xA0, 0x2B),
    yellow: (0xDF, 0x8E, 0x1D),
    blue: (0x1E, 0x66, 0xF5),
    magenta: (0x88, 0x39, 0xEF),
};

const SOLARIZED_DARK: Palette = Palette {
    background: (0x00, 0x2B, 0x36),
    foreground: (0x83, 0x94, 0x96),
    muted: (0x58, 0x6E, 0x75),
    surface: (0x07, 0x36, 0x42),
    green: (0x85, 0x99, 0x00),
    yellow: (0xB5, 0x89, 0x00),
    blue: (0x26, 0x8B, 0xD2),
    magenta: (0xD3, 0x36, 0x82),
};

const SOLARIZED_LIGHT: Palette = Palette {
    background: (0xFD, 0xF6, 0xE3),
    foreground: (0x65, 0x7B, 0x83),
    muted: (0x93, 0xA1, 0xA1),
    surface: (0xEE, 0xE8, 0xD5),
    green: (0x85, 0x99, 0x00),
    yellow: (0xB5, 0x89, 0x00),
    blue: (0x26, 0x8B, 0xD2),
    magenta: (0xD3, 0x36, 0x82),
};

const GRUVBOX_DARK: Palette = Palette {
    background: (0x28, 0x28, 0x28),
    foreground: (0xEB, 0xDB, 0xB2),
    muted: (0x92, 0x83, 0x74),
    surface: (0x3C, 0x38, 0x36),
    green: (0xB8, 0xBB, 0x26),
    yellow: (0xFA, 0xBD, 0x2F),
    blue: (0x83, 0xA5, 0x98),
    magenta: (0xD3, 0x86, 0x9B),
};

const GRUVBOX_LIGHT: Palette = Palette {
    background: (0xFB, 0xF1, 0xC7),
    foreground: (0x3C, 0x38, 0x36),
    muted: (0x7C, 0x6F, 0x64),
    surface: (0xEB, 0xDB, 0xB2),
    green: (0x79, 0x74, 0x0E),
    yellow: (0xB5, 0x76, 0x14),
    blue: (0x07, 0x66, 0x78),
    magenta: (0x8F, 0x3F, 0x71),
};

/// The palette of a non-default theme key.
fn palette_of(theme: ThemeKey) -> &'static Palette {
    match theme {
        ThemeKey::AtomOneDark => &ATOM_ONE_DARK,
        ThemeKey::AtomOneLight => &ATOM_ONE_LIGHT,
        ThemeKey::TokyoNightDark => &TOKYO_NIGHT_DARK,
        ThemeKey::TokyoNightDay => &TOKYO_NIGHT_DAY,
        ThemeKey::CatppuccinMocha => &CATPPUCCIN_MOCHA,
        ThemeKey::CatppuccinLatte => &CATPPUCCIN_LATTE,
        ThemeKey::SolarizedDark => &SOLARIZED_DARK,
        ThemeKey::SolarizedLight => &SOLARIZED_LIGHT,
        ThemeKey::GruvboxDark => &GRUVBOX_DARK,
        ThemeKey::GruvboxLight => &GRUVBOX_LIGHT,
        ThemeKey::Dark => unreachable!("the default theme is Theme::DARK, not a palette"),
    }
}

impl Palette {
    /// Maps the palette's roles onto every semantic field of the model. The
    /// same rule serves dark and light variants, so the renderers stay
    /// variant-agnostic. Text on coloured chips, the cursor and buttons takes
    /// the background colour for contrast in both directions.
    fn theme(&self, truecolor: bool) -> Theme {
        let colour = |role: (u8, u8, u8)| {
            if truecolor {
                Color::Rgb(role.0, role.1, role.2)
            } else {
                Color::Indexed(rgb_to_indexed(role))
            }
        };
        let background = colour(self.background);
        let muted = colour(self.muted);
        let surface = colour(self.surface);
        let green = colour(self.green);
        let yellow = colour(self.yellow);
        let blue = colour(self.blue);
        let magenta = colour(self.magenta);
        Theme {
            background,
            foreground: colour(self.foreground),
            chip_stopped: surface,
            chip_running: green,
            chip_planning: magenta,
            chip_discovering: blue,
            chip_stopping: yellow,
            chip_text: background,
            run_mode_chip: surface,
            status_message: yellow,
            status_key: surface,
            status_label: muted,
            agent_text: AgentText::resolve(self.background, truecolor),
            frame_title_builder: green,
            frame_title_planner: magenta,
            frame_title_research: blue,
            frame_title_detail: muted,
            pane_empty: muted,
            task_done: muted,
            task_running: green,
            task_new: yellow,
            tasks_empty: muted,
            dialog_status: yellow,
            cursor_foreground: background,
            cursor_background: blue,
            dialog_hint: muted,
            choice_detail: muted,
            modal_footer: muted,
            button_accent: yellow,
            settings_error: yellow,
            settings_info: muted,
            settings_readonly: muted,
            settings_help: muted,
            scrollbar_thumb: blue,
            scrollbar_rail: muted,
            rail_done: green,
            rail_active: blue,
            rail_muted: muted,
            rail_connector: muted,
        }
    }
}

/// The nearest xterm-256 index for an RGB triple: the 16 base colours, the
/// 6x6x6 colour cube (levels 0, 95, 135, 175, 215, 255) and the 24 grays
/// (8..=238 in steps of 10), compared by squared RGB distance. The lowest index
/// wins ties, so exact members of the table always map to themselves.
fn rgb_to_indexed(rgb: (u8, u8, u8)) -> u8 {
    let base = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    let cube_level = |i: u8| [0, 95, 135, 175, 215, 255][usize::from(i)];
    let gray = |i: u8| 8 + 10 * i;
    (0..=255)
        .map(|index| {
            let entry = if index < 16 {
                base[usize::from(index)]
            } else if index < 232 {
                let rest = index - 16;
                (
                    cube_level(rest / 36),
                    cube_level((rest % 36) / 6),
                    cube_level(rest % 6),
                )
            } else {
                let g = gray(index - 232);
                (g, g, g)
            };
            let d = |channel: u8, target: u8| i32::from(channel) - i32::from(target);
            let distance =
                d(entry.0, rgb.0).pow(2) + d(entry.1, rgb.1).pow(2) + d(entry.2, rgb.2).pow(2);
            (distance, index)
        })
        .min_by_key(|&(distance, index)| (distance, index))
        .map(|(_, index)| index)
        .unwrap_or(0)
}

/// Whether the terminal advertises 24-bit colour through `COLORTERM`, used when
/// the truecolor setting is in its "auto" state.
fn truecolor_detected() -> bool {
    std::env::var("COLORTERM")
        .map(|value| {
            let value = value.to_lowercase();
            value.contains("truecolor") || value.contains("24bit")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use patok_core::config::THEME_KEYS;

    /// DARK is value-locked to the pre-theme look: every semantic field keeps
    /// the colour the renderers hardcoded before T34.1, and the base style is
    /// the terminal default (Reset/Reset), so painting it is a no-op.
    #[test]
    fn dark_theme_is_the_current_look() {
        let dark = Theme::DARK;
        assert_eq!(dark.background, Color::Reset);
        assert_eq!(dark.foreground, Color::Reset);
        assert_eq!(dark.chip_stopped, Color::DarkGray);
        assert_eq!(dark.chip_running, Color::Green);
        assert_eq!(dark.chip_planning, Color::Magenta);
        assert_eq!(dark.chip_discovering, Color::Blue);
        assert_eq!(dark.chip_stopping, Color::Yellow);
        assert_eq!(dark.chip_text, Color::Black);
        assert_eq!(dark.run_mode_chip, Color::DarkGray);
        assert_eq!(dark.status_message, Color::Yellow);
        assert_eq!(dark.status_key, Color::DarkGray);
        assert_eq!(dark.status_label, Color::DarkGray);
        assert_eq!(dark.agent_text.thinking, Color::Blue);
        assert_eq!(dark.agent_text.tool, Color::Cyan);
        assert_eq!(dark.agent_text.result, Color::DarkGray);
        assert_eq!(dark.agent_text.error, Color::Red);
        assert_eq!(dark.agent_text.notice, Color::Yellow);
        assert_eq!(dark.agent_text.pane_status, Color::Magenta);
        assert_eq!(dark.agent_text.heading, Color::Magenta);
        assert_eq!(dark.frame_title_builder, Color::Green);
        assert_eq!(dark.frame_title_planner, Color::Magenta);
        assert_eq!(dark.frame_title_research, Color::LightBlue);
        assert_eq!(dark.frame_title_detail, Color::DarkGray);
        assert_eq!(dark.pane_empty, Color::DarkGray);
        assert_eq!(dark.task_done, Color::DarkGray);
        assert_eq!(dark.task_running, Color::Green);
        assert_eq!(dark.task_new, Color::Yellow);
        assert_eq!(dark.tasks_empty, Color::DarkGray);
        assert_eq!(dark.dialog_status, Color::Yellow);
        assert_eq!(dark.cursor_foreground, Color::Black);
        assert_eq!(dark.cursor_background, Color::Cyan);
        assert_eq!(dark.dialog_hint, Color::DarkGray);
        assert_eq!(dark.choice_detail, Color::DarkGray);
        assert_eq!(dark.modal_footer, Color::DarkGray);
        assert_eq!(dark.button_accent, Color::Yellow);
        assert_eq!(dark.settings_error, Color::Yellow);
        assert_eq!(dark.settings_info, Color::DarkGray);
        assert_eq!(dark.settings_readonly, Color::DarkGray);
        assert_eq!(dark.settings_help, Color::DarkGray);
        assert_eq!(dark.scrollbar_thumb, Color::Cyan);
        assert_eq!(dark.scrollbar_rail, Color::DarkGray);
        assert_eq!(dark.rail_done, Color::Green);
        assert_eq!(dark.rail_active, Color::Cyan);
        assert_eq!(dark.rail_muted, Color::DarkGray);
        assert_eq!(dark.rail_connector, Color::DarkGray);
    }

    /// Every semantic field of a theme, in a fixed order, for completeness
    /// checks.
    fn every_field(theme: &Theme) -> Vec<Color> {
        vec![
            theme.background,
            theme.foreground,
            theme.chip_stopped,
            theme.chip_running,
            theme.chip_planning,
            theme.chip_discovering,
            theme.chip_stopping,
            theme.chip_text,
            theme.run_mode_chip,
            theme.status_message,
            theme.status_key,
            theme.status_label,
            theme.agent_text.thinking,
            theme.agent_text.tool,
            theme.agent_text.result,
            theme.agent_text.error,
            theme.agent_text.notice,
            theme.agent_text.pane_status,
            theme.agent_text.heading,
            theme.frame_title_builder,
            theme.frame_title_planner,
            theme.frame_title_research,
            theme.frame_title_detail,
            theme.pane_empty,
            theme.task_done,
            theme.task_running,
            theme.task_new,
            theme.tasks_empty,
            theme.dialog_status,
            theme.cursor_foreground,
            theme.cursor_background,
            theme.dialog_hint,
            theme.choice_detail,
            theme.modal_footer,
            theme.button_accent,
            theme.settings_error,
            theme.settings_info,
            theme.settings_readonly,
            theme.settings_help,
            theme.scrollbar_thumb,
            theme.scrollbar_rail,
            theme.rail_done,
            theme.rail_active,
            theme.rail_muted,
            theme.rail_connector,
        ]
    }

    /// Every built-in theme variant, in `THEME_KEYS` order.
    fn every_key() -> Vec<ThemeKey> {
        THEME_KEYS
            .iter()
            .map(|key| ThemeKey::parse(key).expect("THEME_KEYS holds valid names"))
            .collect()
    }

    /// Every built-in theme except the default instantiates with every semantic
    /// field set: no field is left at `Reset`, truecolor emits RGB everywhere and
    /// the 256-colour mode emits indexed colours everywhere. (The default is
    /// value-locked separately, with a `Reset` base so painting it is a no-op.)
    #[test]
    fn every_builtin_theme_sets_every_field() {
        for key in every_key().into_iter().filter(|key| *key != ThemeKey::Dark) {
            let rgb = Theme::resolve(key, Some(true));
            let indexed = Theme::resolve(key, Some(false));
            for theme in [rgb, indexed] {
                for colour in every_field(&theme) {
                    assert_ne!(colour, Color::Reset, "{key:?} left a field unset");
                }
            }
            for colour in every_field(&rgb) {
                assert!(
                    matches!(colour, Color::Rgb(..)),
                    "{key:?} truecolor must emit RGB, got {colour:?}"
                );
            }
            for colour in every_field(&indexed) {
                assert!(
                    matches!(colour, Color::Indexed(..)),
                    "{key:?} 256-colour mode must emit indexed colours, got {colour:?}"
                );
            }
        }
    }

    /// All eleven resolved themes are pairwise distinct: picking any of the
    /// names really changes the colours.
    #[test]
    fn every_builtin_theme_is_distinct() {
        let themes: Vec<Theme> = every_key()
            .into_iter()
            .map(|key| Theme::resolve(key, Some(true)))
            .collect();
        for (i, left) in themes.iter().enumerate() {
            for right in &themes[i + 1..] {
                assert_ne!(left, right, "two built-in themes resolve identically");
            }
        }
    }

    /// The agent line kinds keep the same colour identity in every built-in
    /// theme (T108.1): the default stays on its value-locked named ANSI
    /// colours, and every palette emits each kind's canonical anchor hue —
    /// error red, notice yellow, tool cyan, thinking blue, heading and pane
    /// status magenta — with only the anchor's lightness adjusted to the
    /// palette's background. The `result` kind stays a near-neutral gray and
    /// the lifecycle status lines keep matching the headings (T96.1) in every
    /// theme. Identity is asserted in truecolor mode; the 256-colour mode
    /// derives from the same RGB through the already-tested `rgb_to_indexed`
    /// and may quantize the hue.
    #[test]
    fn agent_text_identity_is_fixed_across_themes() {
        for key in every_key() {
            let theme = Theme::resolve(key, Some(true));
            assert_eq!(
                theme.agent_text.heading, theme.agent_text.pane_status,
                "{key:?} must render status lines in the heading colour"
            );
            if key == ThemeKey::Dark {
                assert_eq!(Theme::line_color(theme, LineKind::Thinking), Color::Blue);
                assert_eq!(Theme::line_color(theme, LineKind::Tool), Color::Cyan);
                assert_eq!(Theme::line_color(theme, LineKind::Result), Color::DarkGray);
                assert_eq!(Theme::line_color(theme, LineKind::Error), Color::Red);
                assert_eq!(Theme::line_color(theme, LineKind::Notice), Color::Yellow);
                assert_eq!(Theme::line_color(theme, LineKind::Status), Color::Magenta);
                assert_eq!(Theme::line_color(theme, LineKind::Heading), Color::Magenta);
                continue;
            }
            // Byte rounding can nudge the recomputed hue by a degree or two,
            // so the tolerance covers quantization, not a real remapping: the
            // anchor hues are more than thirty degrees apart.
            for (kind, anchor) in [
                (LineKind::Thinking, THINKING_ANCHOR),
                (LineKind::Tool, TOOL_ANCHOR),
                (LineKind::Error, ERROR_ANCHOR),
                (LineKind::Notice, NOTICE_ANCHOR),
                (LineKind::Status, HEADING_ANCHOR),
                (LineKind::Heading, HEADING_ANCHOR),
            ] {
                let Color::Rgb(r, g, b) = Theme::line_color(theme, kind) else {
                    panic!("{key:?} must emit RGB in truecolor mode");
                };
                let hue = rgb_to_hsl((r, g, b)).0;
                let anchor_hue = rgb_to_hsl(anchor).0;
                let drift = (hue - anchor_hue)
                    .abs()
                    .min(360.0 - (hue - anchor_hue).abs());
                assert!(
                    drift <= 2.0,
                    "{key:?} {kind:?} drifted {drift:.1} degrees off its anchor hue"
                );
            }
            let Color::Rgb(r, g, b) = Theme::line_color(theme, LineKind::Result) else {
                panic!("{key:?} must emit RGB in truecolor mode");
            };
            let spread = r.max(g).max(b) - r.min(g).min(b);
            assert!(
                spread <= 8,
                "{key:?} result must stay a neutral gray, got ({r}, {g}, {b})"
            );
        }
    }

    /// Every agent line colour stays readable on its theme's background
    /// (T108.1): after the palette's lightness adjustment each of the seven
    /// kinds reaches the WCAG contrast threshold and never equals the
    /// background. The default theme is excluded because its background is the
    /// terminal default (`Color::Reset`), whose colour the theme cannot know;
    /// its identity is value-locked by the DARK test above. Asserted in
    /// truecolor mode; the 256-colour mode derives from the same RGB.
    #[test]
    fn agent_text_is_readable_on_every_theme_background() {
        for key in every_key().into_iter().filter(|key| *key != ThemeKey::Dark) {
            let theme = Theme::resolve(key, Some(true));
            let Color::Rgb(r, g, b) = theme.background else {
                panic!("{key:?} palettes always emit RGB backgrounds");
            };
            let background = (r, g, b);
            for kind in [
                LineKind::Thinking,
                LineKind::Tool,
                LineKind::Result,
                LineKind::Error,
                LineKind::Notice,
                LineKind::Status,
                LineKind::Heading,
            ] {
                let Color::Rgb(r, g, b) = Theme::line_color(theme, kind) else {
                    panic!("{key:?} must emit RGB in truecolor mode");
                };
                let line = (r, g, b);
                assert_ne!(
                    line, background,
                    "{key:?} {kind:?} must differ from the theme background"
                );
                let ratio = contrast_ratio(line, background);
                assert!(
                    ratio >= READABLE_CONTRAST,
                    "{key:?} {kind:?} reaches only {ratio} against the background"
                );
            }
        }
    }

    /// The theme names round-trip through `ThemeKey::parse`/`as_str` with no
    /// duplicates, so the config key, the persistence validator and the
    /// overlay's cycle agree on the exact list.
    #[test]
    fn theme_names_round_trip_with_no_duplicates() {
        assert_eq!(THEME_KEYS.len(), 11);
        for (i, key) in THEME_KEYS.iter().enumerate() {
            assert!(
                !THEME_KEYS[..i].contains(key),
                "{key} appears twice in THEME_KEYS"
            );
            let variant = ThemeKey::parse(key).expect("a valid name");
            assert_eq!(variant.as_str(), *key);
        }
        assert_eq!(THEME_KEYS.first().copied(), Some("dark"));
        assert_eq!(ThemeKey::parse("neon"), None);
        // The pre-T36.1 placeholder names are no longer valid.
        assert_eq!(ThemeKey::parse("catppuccin"), None);
        assert_eq!(ThemeKey::parse("solarized"), None);
    }

    /// The nearest-index mapping hits exact members of the xterm-256 table and
    /// picks the nearest neighbour for everything else.
    #[test]
    fn rgb_to_indexed_hits_exact_entries_and_nearest_neighbours() {
        // Exact base colours.
        assert_eq!(rgb_to_indexed((0, 0, 0)), 0);
        assert_eq!(rgb_to_indexed((255, 255, 255)), 15);
        // An exact colour-cube entry: levels 95, 135, 175.
        assert_eq!(rgb_to_indexed((95, 135, 175)), 16 + 36 + 2 * 6 + 3);
        // Exact grays at both ends of the ramp.
        assert_eq!(rgb_to_indexed((8, 8, 8)), 232);
        assert_eq!(rgb_to_indexed((238, 238, 238)), 255);
        // Nearest neighbours: a mid gray snaps to the ramp, an off-white keeps
        // the bright gray rather than the base white.
        assert_eq!(rgb_to_indexed((100, 100, 100)), 241);
        assert_eq!(rgb_to_indexed((240, 240, 240)), 255);
    }
}
