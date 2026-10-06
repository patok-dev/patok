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
//! [`Palette`] of signature roles and carries its own [`Chrome`] table that
//! maps those roles onto the semantic fields (T109.1), so each theme tunes its
//! chrome — chips, cursor, muted text — without touching the others, and every
//! renderer works unchanged with any variant. RGB colours emit as `Color::Rgb`
//! while the truecolor setting is on and as the nearest xterm-256 indexed
//! colour while it is off.
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
//! The agent *name* colours are fixated the same way (T110.1): each agent
//! (planner, builder, reviewer, research, discovery, orchestrator) keeps one
//! hue family in every built-in theme, held in the [`AgentNames`] subclass
//! and reached through [`Theme::agent_name_color`]. A palette only adjusts
//! an anchor's lightness so the name reads on its background; the identity
//! never remaps onto a palette role. Every surface that renders an agent by
//! name -- the output frame title, the lifecycle status lines, the planning
//! heading, the status bar's run message and the headless `--color` output --
//! routes through the same mapping.
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
//! | `agent_names.*` | `render_output`, `visual_lines`, `status_widget`, headless `--color` | every agent name rendered by identity (T110.1): the output frame title's agent type name, the agent name inside the lifecycle started/finished lines, the planning heading's name and the status bar's "{Agent} running..." message |
//! | `frame_title_detail` | `render_output`, `render_tasks` | output frame title's separator, provider, model and timer; everything after the word `Tasks` in the tasks frame title (T67.1): the pipe separator, the completed, total and left counts, the slash, the dash and the word `left` |
//! | `agent_text.pane_status` | `style_of`, headless `--color` | the agent lifecycle status lines in the output pane (T78.1, T42.1) outside the agent name itself, which wears its own fixed colour (T110.1) |
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

/// The agent name colours (T110.1): one fixed colour identity per agent,
/// the same in every built-in theme. Like the line kinds, a palette only
/// adjusts an anchor's lightness so the name reads on its background; the
/// hue family never remaps onto a palette role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentNames {
    /// The planner's name: magenta.
    pub planner: Color,
    /// The builder's name: green.
    pub builder: Color,
    /// The reviewer's name: orange.
    pub reviewer: Color,
    /// The research agent's name: blue.
    pub research: Color,
    /// The discovery agent's name: cyan.
    pub discovery: Color,
    /// The orchestrator's name: rose.
    pub orchestrator: Color,
    /// Any other agent's name: the neutral gray, like the result lines.
    pub other: Color,
}

/// The canonical anchor colours of the agent names (T110.1): one RGB triple
/// per agent, fixed for every built-in theme. Planner, research and discovery
/// reuse the line-kind family anchors so the fixated identities match the
/// default theme's existing look; reviewer and orchestrator take families the
/// kinds do not use. The chromatic anchors sit more than 34 degrees apart on
/// the hue wheel, so no two agents can ever be confused.
/// The planner's name: magenta, the heading family.
const PLANNER_NAME_ANCHOR: (u8, u8, u8) = HEADING_ANCHOR;
/// The builder's name: green.
const BUILDER_NAME_ANCHOR: (u8, u8, u8) = (0x8A, 0xC9, 0x5A);
/// The reviewer's name: orange.
const REVIEWER_NAME_ANCHOR: (u8, u8, u8) = (0xE0, 0x84, 0x3D);
/// The research agent's name: blue, the thinking family.
const RESEARCH_NAME_ANCHOR: (u8, u8, u8) = THINKING_ANCHOR;
/// The discovery agent's name: cyan, the tool family.
const DISCOVERY_NAME_ANCHOR: (u8, u8, u8) = TOOL_ANCHOR;
/// The orchestrator's name: rose.
const ORCHESTRATOR_NAME_ANCHOR: (u8, u8, u8) = (0xE0, 0x6C, 0x9A);
/// Any other agent's name: the neutral gray, the result family.
const OTHER_NAME_ANCHOR: (u8, u8, u8) = RESULT_ANCHOR;

impl AgentNames {
    /// Resolves the agent name colours for a palette theme (T110.1): every
    /// agent keeps its canonical anchor; the theme's background only adjusts
    /// each anchor's lightness until the pair reads, exactly like the agent
    /// line kinds (T108.1). The truecolor setting picks the emission.
    fn resolve(background: (u8, u8, u8), truecolor: bool) -> AgentNames {
        let colour = |rgb: (u8, u8, u8)| {
            if truecolor {
                Color::Rgb(rgb.0, rgb.1, rgb.2)
            } else {
                Color::Indexed(rgb_to_indexed(rgb))
            }
        };
        let name = |anchor: (u8, u8, u8)| colour(adjust_lightness(anchor, background));
        AgentNames {
            planner: name(PLANNER_NAME_ANCHOR),
            builder: name(BUILDER_NAME_ANCHOR),
            reviewer: name(REVIEWER_NAME_ANCHOR),
            research: name(RESEARCH_NAME_ANCHOR),
            discovery: name(DISCOVERY_NAME_ANCHOR),
            orchestrator: name(ORCHESTRATOR_NAME_ANCHOR),
            other: name(OTHER_NAME_ANCHOR),
        }
    }
}

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
    /// The agent name colours (T110.1): one fixed identity per agent, the
    /// same hue family in every built-in theme.
    pub agent_names: AgentNames,
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
        agent_names: AgentNames {
            planner: Color::Magenta,
            builder: Color::Green,
            reviewer: Color::LightYellow,
            research: Color::LightBlue,
            discovery: Color::Cyan,
            orchestrator: Color::LightMagenta,
            other: Color::DarkGray,
        },
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
            other => palette_of(other).theme(
                chrome_of(other),
                truecolor.unwrap_or_else(truecolor_detected),
            ),
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

    /// The fixed colour of one agent's name (T110.1): the same hue family in
    /// every built-in theme, only adjusted for readability on the theme's
    /// background. Unknown agents fall back to the neutral `other` colour.
    /// Shared by the frame's agent-name surfaces and the headless mode's
    /// `--color` output, so the two cannot drift.
    pub fn agent_name_color(theme: Theme, agent: &str) -> Color {
        match agent {
            "planner" => theme.agent_names.planner,
            "builder" => theme.agent_names.builder,
            "reviewer" => theme.agent_names.reviewer,
            "research" => theme.agent_names.research,
            "discovery" => theme.agent_names.discovery,
            "orchestrator" => theme.agent_names.orchestrator,
            _ => theme.agent_names.other,
        }
    }
}

/// One published palette's signature roles, as raw RGB triples. Each palette
/// has its own [`Chrome`] table ([`Palette::theme`]) that maps the roles onto
/// the semantic fields, which is what keeps every renderer variant-agnostic.
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

/// One theme's chrome mapping (T109.1): the raw RGB triple every
/// non-fixated semantic field of [`Theme`] resolves to, in the same order
/// as the `Theme` fields minus the fixated subclasses. Each built-in palette owns one
/// table — there is no shared mapping rule — so a theme can tune any chrome
/// use (chip backgrounds, cursor, muted text) without touching the others.
/// The agent line colours and agent name colours are fixated separately by
/// [`AgentText`] (T108.1) and [`AgentNames`] (T110.1) and never come from
/// here.
struct Chrome {
    /// The shell's base background, painted over the whole frame first.
    background: (u8, u8, u8),
    /// The shell's base foreground, inherited by every unstyled text cell.
    foreground: (u8, u8, u8),
    /// Status-line STOPPED status chip background.
    chip_stopped: (u8, u8, u8),
    /// Status-line RUNNING status chip background.
    chip_running: (u8, u8, u8),
    /// Status-line PLANNING status chip background.
    chip_planning: (u8, u8, u8),
    /// Status-line DISCOVERING status chip background.
    chip_discovering: (u8, u8, u8),
    /// Status-line STOPPING status chip background.
    chip_stopping: (u8, u8, u8),
    /// Foreground on every status-line status chip.
    chip_text: (u8, u8, u8),
    /// Run-mode chip background (foreground `chip_text`).
    run_mode_chip: (u8, u8, u8),
    /// The status bar's one-line message.
    status_message: (u8, u8, u8),
    /// Status-bar key-chip background (foreground `chip_text`).
    status_key: (u8, u8, u8),
    /// Status-bar key-chip labels.
    status_label: (u8, u8, u8),
    /// The output frame title's detail text (T37.1) and everything after the
    /// word `Tasks` in the tasks frame title (T67.1).
    frame_title_detail: (u8, u8, u8),
    /// "no output yet" placeholder in the output pane.
    pane_empty: (u8, u8, u8),
    /// Done task rows.
    task_done: (u8, u8, u8),
    /// Running task rows (the renderer adds bold).
    task_running: (u8, u8, u8),
    /// New-task highlight (the renderer adds bold + reversed).
    task_new: (u8, u8, u8),
    /// "no tasks in TASKS.md" placeholder.
    tasks_empty: (u8, u8, u8),
    /// The add-task dialog's status line.
    dialog_status: (u8, u8, u8),
    /// The dialog input's block cursor foreground.
    cursor_foreground: (u8, u8, u8),
    /// The dialog input's block cursor background.
    cursor_background: (u8, u8, u8),
    /// The empty-input watermark (T41.1).
    dialog_hint: (u8, u8, u8),
    /// "-- detail" text in the stop and unsaved-changes dialogs.
    choice_detail: (u8, u8, u8),
    /// Every modal bottom line's key-hint zone (T59.1).
    modal_footer: (u8, u8, u8),
    /// Every modal bottom line's " [ Key ] Label " buttons and the settings
    /// overlay's " [ X ] " close marker (T44.1, T59.1).
    button_accent: (u8, u8, u8),
    /// The settings overlay's error status line.
    settings_error: (u8, u8, u8),
    /// The settings overlay's info status line.
    settings_info: (u8, u8, u8),
    /// Read-only settings overlay rows.
    settings_readonly: (u8, u8, u8),
    /// The settings overlay's help box text (T85.1).
    settings_help: (u8, u8, u8),
    /// The settings overlay's scrollbar thumb.
    scrollbar_thumb: (u8, u8, u8),
    /// The settings overlay's scrollbar rail.
    scrollbar_rail: (u8, u8, u8),
    /// Done rail tiles (T50.1): the stages before the active one,
    /// SHIP while shipping, DISCOVER after a round ran, LEARNINGS once learned.
    rail_done: (u8, u8, u8),
    /// The active stage tile and DISCOVER while a round runs (bold added).
    rail_active: (u8, u8, u8),
    /// Muted and pending rail tiles.
    rail_muted: (u8, u8, u8),
    /// The rail's down-arrow connectors between consecutive stage tiles.
    rail_connector: (u8, u8, u8),
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

/// The neutral chips (stopped, run-mode, key) take the foreground role: the
/// background-role chip text only reads on chip backgrounds at least as
/// bright as the accents, and the surface role is not. The palette's muted
/// (2.3:1 on the background) is dimmer than the benchmark theme's, so every
/// text-bearing muted use takes a lightened muted literal that clears
/// [`READABLE_CONTRAST`]; the decorative scrollbar rail keeps the palette
/// value.
const ATOM_ONE_DARK_CHROME: Chrome = Chrome {
    background: ATOM_ONE_DARK.background,
    foreground: ATOM_ONE_DARK.foreground,
    chip_stopped: ATOM_ONE_DARK.foreground,
    chip_running: ATOM_ONE_DARK.green,
    chip_planning: ATOM_ONE_DARK.magenta,
    chip_discovering: ATOM_ONE_DARK.blue,
    chip_stopping: ATOM_ONE_DARK.yellow,
    chip_text: ATOM_ONE_DARK.background,
    run_mode_chip: ATOM_ONE_DARK.foreground,
    status_message: ATOM_ONE_DARK.yellow,
    status_key: ATOM_ONE_DARK.foreground,
    status_label: (0x6E, 0x77, 0x86),
    frame_title_detail: (0x6E, 0x77, 0x86),
    pane_empty: (0x6E, 0x77, 0x86),
    task_done: (0x6E, 0x77, 0x86),
    task_running: ATOM_ONE_DARK.green,
    task_new: ATOM_ONE_DARK.yellow,
    tasks_empty: (0x6E, 0x77, 0x86),
    dialog_status: ATOM_ONE_DARK.yellow,
    cursor_foreground: ATOM_ONE_DARK.background,
    cursor_background: ATOM_ONE_DARK.blue,
    dialog_hint: (0x6E, 0x77, 0x86),
    choice_detail: (0x6E, 0x77, 0x86),
    modal_footer: (0x6E, 0x77, 0x86),
    button_accent: ATOM_ONE_DARK.yellow,
    settings_error: ATOM_ONE_DARK.yellow,
    settings_info: (0x6E, 0x77, 0x86),
    settings_readonly: (0x6E, 0x77, 0x86),
    settings_help: (0x6E, 0x77, 0x86),
    scrollbar_thumb: ATOM_ONE_DARK.blue,
    scrollbar_rail: ATOM_ONE_DARK.muted,
    rail_done: ATOM_ONE_DARK.green,
    rail_active: ATOM_ONE_DARK.blue,
    rail_muted: (0x6E, 0x77, 0x86),
    rail_connector: (0x6E, 0x77, 0x86),
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

/// Light theme: the neutral chips take the foreground role so the
/// background-role chip text (near-white) reads on them, and every
/// text-bearing muted use takes a darkened muted literal that clears
/// [`READABLE_CONTRAST`] on the near-white background.
const ATOM_ONE_LIGHT_CHROME: Chrome = Chrome {
    background: ATOM_ONE_LIGHT.background,
    foreground: ATOM_ONE_LIGHT.foreground,
    chip_stopped: ATOM_ONE_LIGHT.foreground,
    chip_running: ATOM_ONE_LIGHT.green,
    chip_planning: ATOM_ONE_LIGHT.magenta,
    chip_discovering: ATOM_ONE_LIGHT.blue,
    chip_stopping: ATOM_ONE_LIGHT.yellow,
    chip_text: ATOM_ONE_LIGHT.background,
    run_mode_chip: ATOM_ONE_LIGHT.foreground,
    status_message: ATOM_ONE_LIGHT.yellow,
    status_key: ATOM_ONE_LIGHT.foreground,
    status_label: (0x86, 0x87, 0x8E),
    frame_title_detail: (0x86, 0x87, 0x8E),
    pane_empty: (0x86, 0x87, 0x8E),
    task_done: (0x86, 0x87, 0x8E),
    task_running: ATOM_ONE_LIGHT.green,
    task_new: ATOM_ONE_LIGHT.yellow,
    tasks_empty: (0x86, 0x87, 0x8E),
    dialog_status: ATOM_ONE_LIGHT.yellow,
    cursor_foreground: ATOM_ONE_LIGHT.background,
    cursor_background: ATOM_ONE_LIGHT.blue,
    dialog_hint: (0x86, 0x87, 0x8E),
    choice_detail: (0x86, 0x87, 0x8E),
    modal_footer: (0x86, 0x87, 0x8E),
    button_accent: ATOM_ONE_LIGHT.yellow,
    settings_error: ATOM_ONE_LIGHT.yellow,
    settings_info: (0x86, 0x87, 0x8E),
    settings_readonly: (0x86, 0x87, 0x8E),
    settings_help: (0x86, 0x87, 0x8E),
    scrollbar_thumb: ATOM_ONE_LIGHT.blue,
    scrollbar_rail: ATOM_ONE_LIGHT.muted,
    rail_done: ATOM_ONE_LIGHT.green,
    rail_active: ATOM_ONE_LIGHT.blue,
    rail_muted: (0x86, 0x87, 0x8E),
    rail_connector: (0x86, 0x87, 0x8E),
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

/// The benchmark theme (T109.1): its look is snapshot-locked and byte-identical
/// to the output of the single shared rule this table replaced.
const TOKYO_NIGHT_DARK_CHROME: Chrome = Chrome {
    background: TOKYO_NIGHT_DARK.background,
    foreground: TOKYO_NIGHT_DARK.foreground,
    chip_stopped: TOKYO_NIGHT_DARK.surface,
    chip_running: TOKYO_NIGHT_DARK.green,
    chip_planning: TOKYO_NIGHT_DARK.magenta,
    chip_discovering: TOKYO_NIGHT_DARK.blue,
    chip_stopping: TOKYO_NIGHT_DARK.yellow,
    chip_text: TOKYO_NIGHT_DARK.background,
    run_mode_chip: TOKYO_NIGHT_DARK.surface,
    status_message: TOKYO_NIGHT_DARK.yellow,
    status_key: TOKYO_NIGHT_DARK.surface,
    status_label: TOKYO_NIGHT_DARK.muted,
    frame_title_detail: TOKYO_NIGHT_DARK.muted,
    pane_empty: TOKYO_NIGHT_DARK.muted,
    task_done: TOKYO_NIGHT_DARK.muted,
    task_running: TOKYO_NIGHT_DARK.green,
    task_new: TOKYO_NIGHT_DARK.yellow,
    tasks_empty: TOKYO_NIGHT_DARK.muted,
    dialog_status: TOKYO_NIGHT_DARK.yellow,
    cursor_foreground: TOKYO_NIGHT_DARK.background,
    cursor_background: TOKYO_NIGHT_DARK.blue,
    dialog_hint: TOKYO_NIGHT_DARK.muted,
    choice_detail: TOKYO_NIGHT_DARK.muted,
    modal_footer: TOKYO_NIGHT_DARK.muted,
    button_accent: TOKYO_NIGHT_DARK.yellow,
    settings_error: TOKYO_NIGHT_DARK.yellow,
    settings_info: TOKYO_NIGHT_DARK.muted,
    settings_readonly: TOKYO_NIGHT_DARK.muted,
    settings_help: TOKYO_NIGHT_DARK.muted,
    scrollbar_thumb: TOKYO_NIGHT_DARK.blue,
    scrollbar_rail: TOKYO_NIGHT_DARK.muted,
    rail_done: TOKYO_NIGHT_DARK.green,
    rail_active: TOKYO_NIGHT_DARK.blue,
    rail_muted: TOKYO_NIGHT_DARK.muted,
    rail_connector: TOKYO_NIGHT_DARK.muted,
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

/// Light theme: the neutral chips take the foreground role so the
/// background-role chip text reads on them, and every text-bearing muted use
/// takes a darkened muted literal that clears [`READABLE_CONTRAST`].
const TOKYO_NIGHT_DAY_CHROME: Chrome = Chrome {
    background: TOKYO_NIGHT_DAY.background,
    foreground: TOKYO_NIGHT_DAY.foreground,
    chip_stopped: TOKYO_NIGHT_DAY.foreground,
    chip_running: TOKYO_NIGHT_DAY.green,
    chip_planning: TOKYO_NIGHT_DAY.magenta,
    chip_discovering: TOKYO_NIGHT_DAY.blue,
    chip_stopping: TOKYO_NIGHT_DAY.yellow,
    chip_text: TOKYO_NIGHT_DAY.background,
    run_mode_chip: TOKYO_NIGHT_DAY.foreground,
    status_message: TOKYO_NIGHT_DAY.yellow,
    status_key: TOKYO_NIGHT_DAY.foreground,
    status_label: (0x71, 0x7A, 0xAA),
    frame_title_detail: (0x71, 0x7A, 0xAA),
    pane_empty: (0x71, 0x7A, 0xAA),
    task_done: (0x71, 0x7A, 0xAA),
    task_running: TOKYO_NIGHT_DAY.green,
    task_new: TOKYO_NIGHT_DAY.yellow,
    tasks_empty: (0x71, 0x7A, 0xAA),
    dialog_status: TOKYO_NIGHT_DAY.yellow,
    cursor_foreground: TOKYO_NIGHT_DAY.background,
    cursor_background: TOKYO_NIGHT_DAY.blue,
    dialog_hint: (0x71, 0x7A, 0xAA),
    choice_detail: (0x71, 0x7A, 0xAA),
    modal_footer: (0x71, 0x7A, 0xAA),
    button_accent: TOKYO_NIGHT_DAY.yellow,
    settings_error: TOKYO_NIGHT_DAY.yellow,
    settings_info: (0x71, 0x7A, 0xAA),
    settings_readonly: (0x71, 0x7A, 0xAA),
    settings_help: (0x71, 0x7A, 0xAA),
    scrollbar_thumb: TOKYO_NIGHT_DAY.blue,
    scrollbar_rail: TOKYO_NIGHT_DAY.muted,
    rail_done: TOKYO_NIGHT_DAY.green,
    rail_active: TOKYO_NIGHT_DAY.blue,
    rail_muted: (0x71, 0x7A, 0xAA),
    rail_connector: (0x71, 0x7A, 0xAA),
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

/// Only the neutral chips move to the foreground role; Mocha's own muted and
/// accents already clear [`READABLE_CONTRAST`] on its background.
const CATPPUCCIN_MOCHA_CHROME: Chrome = Chrome {
    background: CATPPUCCIN_MOCHA.background,
    foreground: CATPPUCCIN_MOCHA.foreground,
    chip_stopped: CATPPUCCIN_MOCHA.foreground,
    chip_running: CATPPUCCIN_MOCHA.green,
    chip_planning: CATPPUCCIN_MOCHA.magenta,
    chip_discovering: CATPPUCCIN_MOCHA.blue,
    chip_stopping: CATPPUCCIN_MOCHA.yellow,
    chip_text: CATPPUCCIN_MOCHA.background,
    run_mode_chip: CATPPUCCIN_MOCHA.foreground,
    status_message: CATPPUCCIN_MOCHA.yellow,
    status_key: CATPPUCCIN_MOCHA.foreground,
    status_label: CATPPUCCIN_MOCHA.muted,
    frame_title_detail: CATPPUCCIN_MOCHA.muted,
    pane_empty: CATPPUCCIN_MOCHA.muted,
    task_done: CATPPUCCIN_MOCHA.muted,
    task_running: CATPPUCCIN_MOCHA.green,
    task_new: CATPPUCCIN_MOCHA.yellow,
    tasks_empty: CATPPUCCIN_MOCHA.muted,
    dialog_status: CATPPUCCIN_MOCHA.yellow,
    cursor_foreground: CATPPUCCIN_MOCHA.background,
    cursor_background: CATPPUCCIN_MOCHA.blue,
    dialog_hint: CATPPUCCIN_MOCHA.muted,
    choice_detail: CATPPUCCIN_MOCHA.muted,
    modal_footer: CATPPUCCIN_MOCHA.muted,
    button_accent: CATPPUCCIN_MOCHA.yellow,
    settings_error: CATPPUCCIN_MOCHA.yellow,
    settings_info: CATPPUCCIN_MOCHA.muted,
    settings_readonly: CATPPUCCIN_MOCHA.muted,
    settings_help: CATPPUCCIN_MOCHA.muted,
    scrollbar_thumb: CATPPUCCIN_MOCHA.blue,
    scrollbar_rail: CATPPUCCIN_MOCHA.muted,
    rail_done: CATPPUCCIN_MOCHA.green,
    rail_active: CATPPUCCIN_MOCHA.blue,
    rail_muted: CATPPUCCIN_MOCHA.muted,
    rail_connector: CATPPUCCIN_MOCHA.muted,
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

/// Light theme: the neutral chips take the foreground role so the
/// background-role chip text reads on them. Latte's muted already clears
/// [`READABLE_CONTRAST`], but its green and yellow fall short both as chip
/// backgrounds under the chip text and as text on the background, so every
/// green and yellow use takes a darkened literal of the role's hue.
const CATPPUCCIN_LATTE_CHROME: Chrome = Chrome {
    background: CATPPUCCIN_LATTE.background,
    foreground: CATPPUCCIN_LATTE.foreground,
    chip_stopped: CATPPUCCIN_LATTE.foreground,
    chip_running: (0x3D, 0x98, 0x29),
    chip_planning: CATPPUCCIN_LATTE.magenta,
    chip_discovering: CATPPUCCIN_LATTE.blue,
    chip_stopping: (0xBB, 0x77, 0x18),
    chip_text: CATPPUCCIN_LATTE.background,
    run_mode_chip: CATPPUCCIN_LATTE.foreground,
    status_message: (0xBB, 0x77, 0x18),
    status_key: CATPPUCCIN_LATTE.foreground,
    status_label: CATPPUCCIN_LATTE.muted,
    frame_title_detail: CATPPUCCIN_LATTE.muted,
    pane_empty: CATPPUCCIN_LATTE.muted,
    task_done: CATPPUCCIN_LATTE.muted,
    task_running: (0x3D, 0x98, 0x29),
    task_new: (0xBB, 0x77, 0x18),
    tasks_empty: CATPPUCCIN_LATTE.muted,
    dialog_status: (0xBB, 0x77, 0x18),
    cursor_foreground: CATPPUCCIN_LATTE.background,
    cursor_background: CATPPUCCIN_LATTE.blue,
    dialog_hint: CATPPUCCIN_LATTE.muted,
    choice_detail: CATPPUCCIN_LATTE.muted,
    modal_footer: CATPPUCCIN_LATTE.muted,
    button_accent: (0xBB, 0x77, 0x18),
    settings_error: (0xBB, 0x77, 0x18),
    settings_info: CATPPUCCIN_LATTE.muted,
    settings_readonly: CATPPUCCIN_LATTE.muted,
    settings_help: CATPPUCCIN_LATTE.muted,
    scrollbar_thumb: CATPPUCCIN_LATTE.blue,
    scrollbar_rail: CATPPUCCIN_LATTE.muted,
    rail_done: (0x3D, 0x98, 0x29),
    rail_active: CATPPUCCIN_LATTE.blue,
    rail_muted: CATPPUCCIN_LATTE.muted,
    rail_connector: CATPPUCCIN_LATTE.muted,
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

/// Only the neutral chips move to the foreground role; Solarized Dark's muted
/// sits at the benchmark theme's dimness and its accents already clear
/// [`READABLE_CONTRAST`].
const SOLARIZED_DARK_CHROME: Chrome = Chrome {
    background: SOLARIZED_DARK.background,
    foreground: SOLARIZED_DARK.foreground,
    chip_stopped: SOLARIZED_DARK.foreground,
    chip_running: SOLARIZED_DARK.green,
    chip_planning: SOLARIZED_DARK.magenta,
    chip_discovering: SOLARIZED_DARK.blue,
    chip_stopping: SOLARIZED_DARK.yellow,
    chip_text: SOLARIZED_DARK.background,
    run_mode_chip: SOLARIZED_DARK.foreground,
    status_message: SOLARIZED_DARK.yellow,
    status_key: SOLARIZED_DARK.foreground,
    status_label: SOLARIZED_DARK.muted,
    frame_title_detail: SOLARIZED_DARK.muted,
    pane_empty: SOLARIZED_DARK.muted,
    task_done: SOLARIZED_DARK.muted,
    task_running: SOLARIZED_DARK.green,
    task_new: SOLARIZED_DARK.yellow,
    tasks_empty: SOLARIZED_DARK.muted,
    dialog_status: SOLARIZED_DARK.yellow,
    cursor_foreground: SOLARIZED_DARK.background,
    cursor_background: SOLARIZED_DARK.blue,
    dialog_hint: SOLARIZED_DARK.muted,
    choice_detail: SOLARIZED_DARK.muted,
    modal_footer: SOLARIZED_DARK.muted,
    button_accent: SOLARIZED_DARK.yellow,
    settings_error: SOLARIZED_DARK.yellow,
    settings_info: SOLARIZED_DARK.muted,
    settings_readonly: SOLARIZED_DARK.muted,
    settings_help: SOLARIZED_DARK.muted,
    scrollbar_thumb: SOLARIZED_DARK.blue,
    scrollbar_rail: SOLARIZED_DARK.muted,
    rail_done: SOLARIZED_DARK.green,
    rail_active: SOLARIZED_DARK.blue,
    rail_muted: SOLARIZED_DARK.muted,
    rail_connector: SOLARIZED_DARK.muted,
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

/// Light theme: the neutral chips take the foreground role so the
/// background-role chip text reads on them, every text-bearing muted use
/// takes a darkened muted literal, and the green and yellow — short both as
/// chip backgrounds under the chip text and as text on the background — take
/// darkened literals of their hues.
const SOLARIZED_LIGHT_CHROME: Chrome = Chrome {
    background: SOLARIZED_LIGHT.background,
    foreground: SOLARIZED_LIGHT.foreground,
    chip_stopped: SOLARIZED_LIGHT.foreground,
    chip_running: (0x7C, 0x8F, 0x00),
    chip_planning: SOLARIZED_LIGHT.magenta,
    chip_discovering: SOLARIZED_LIGHT.blue,
    chip_stopping: (0xAB, 0x81, 0x00),
    chip_text: SOLARIZED_LIGHT.background,
    run_mode_chip: SOLARIZED_LIGHT.foreground,
    status_message: (0xAB, 0x81, 0x00),
    status_key: SOLARIZED_LIGHT.foreground,
    status_label: (0x76, 0x87, 0x87),
    frame_title_detail: (0x76, 0x87, 0x87),
    pane_empty: (0x76, 0x87, 0x87),
    task_done: (0x76, 0x87, 0x87),
    task_running: (0x7C, 0x8F, 0x00),
    task_new: (0xAB, 0x81, 0x00),
    tasks_empty: (0x76, 0x87, 0x87),
    dialog_status: (0xAB, 0x81, 0x00),
    cursor_foreground: SOLARIZED_LIGHT.background,
    cursor_background: SOLARIZED_LIGHT.blue,
    dialog_hint: (0x76, 0x87, 0x87),
    choice_detail: (0x76, 0x87, 0x87),
    modal_footer: (0x76, 0x87, 0x87),
    button_accent: (0xAB, 0x81, 0x00),
    settings_error: (0xAB, 0x81, 0x00),
    settings_info: (0x76, 0x87, 0x87),
    settings_readonly: (0x76, 0x87, 0x87),
    settings_help: (0x76, 0x87, 0x87),
    scrollbar_thumb: SOLARIZED_LIGHT.blue,
    scrollbar_rail: SOLARIZED_LIGHT.muted,
    rail_done: (0x7C, 0x8F, 0x00),
    rail_active: SOLARIZED_LIGHT.blue,
    rail_muted: (0x76, 0x87, 0x87),
    rail_connector: (0x76, 0x87, 0x87),
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

/// Only the neutral chips move to the foreground role; Gruvbox Dark's own
/// muted and accents already clear [`READABLE_CONTRAST`] on its background.
const GRUVBOX_DARK_CHROME: Chrome = Chrome {
    background: GRUVBOX_DARK.background,
    foreground: GRUVBOX_DARK.foreground,
    chip_stopped: GRUVBOX_DARK.foreground,
    chip_running: GRUVBOX_DARK.green,
    chip_planning: GRUVBOX_DARK.magenta,
    chip_discovering: GRUVBOX_DARK.blue,
    chip_stopping: GRUVBOX_DARK.yellow,
    chip_text: GRUVBOX_DARK.background,
    run_mode_chip: GRUVBOX_DARK.foreground,
    status_message: GRUVBOX_DARK.yellow,
    status_key: GRUVBOX_DARK.foreground,
    status_label: GRUVBOX_DARK.muted,
    frame_title_detail: GRUVBOX_DARK.muted,
    pane_empty: GRUVBOX_DARK.muted,
    task_done: GRUVBOX_DARK.muted,
    task_running: GRUVBOX_DARK.green,
    task_new: GRUVBOX_DARK.yellow,
    tasks_empty: GRUVBOX_DARK.muted,
    dialog_status: GRUVBOX_DARK.yellow,
    cursor_foreground: GRUVBOX_DARK.background,
    cursor_background: GRUVBOX_DARK.blue,
    dialog_hint: GRUVBOX_DARK.muted,
    choice_detail: GRUVBOX_DARK.muted,
    modal_footer: GRUVBOX_DARK.muted,
    button_accent: GRUVBOX_DARK.yellow,
    settings_error: GRUVBOX_DARK.yellow,
    settings_info: GRUVBOX_DARK.muted,
    settings_readonly: GRUVBOX_DARK.muted,
    settings_help: GRUVBOX_DARK.muted,
    scrollbar_thumb: GRUVBOX_DARK.blue,
    scrollbar_rail: GRUVBOX_DARK.muted,
    rail_done: GRUVBOX_DARK.green,
    rail_active: GRUVBOX_DARK.blue,
    rail_muted: GRUVBOX_DARK.muted,
    rail_connector: GRUVBOX_DARK.muted,
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

/// Only the neutral chips move to the foreground role; Gruvbox Light's own
/// muted and accents already clear [`READABLE_CONTRAST`] on its background.
const GRUVBOX_LIGHT_CHROME: Chrome = Chrome {
    background: GRUVBOX_LIGHT.background,
    foreground: GRUVBOX_LIGHT.foreground,
    chip_stopped: GRUVBOX_LIGHT.foreground,
    chip_running: GRUVBOX_LIGHT.green,
    chip_planning: GRUVBOX_LIGHT.magenta,
    chip_discovering: GRUVBOX_LIGHT.blue,
    chip_stopping: GRUVBOX_LIGHT.yellow,
    chip_text: GRUVBOX_LIGHT.background,
    run_mode_chip: GRUVBOX_LIGHT.foreground,
    status_message: GRUVBOX_LIGHT.yellow,
    status_key: GRUVBOX_LIGHT.foreground,
    status_label: GRUVBOX_LIGHT.muted,
    frame_title_detail: GRUVBOX_LIGHT.muted,
    pane_empty: GRUVBOX_LIGHT.muted,
    task_done: GRUVBOX_LIGHT.muted,
    task_running: GRUVBOX_LIGHT.green,
    task_new: GRUVBOX_LIGHT.yellow,
    tasks_empty: GRUVBOX_LIGHT.muted,
    dialog_status: GRUVBOX_LIGHT.yellow,
    cursor_foreground: GRUVBOX_LIGHT.background,
    cursor_background: GRUVBOX_LIGHT.blue,
    dialog_hint: GRUVBOX_LIGHT.muted,
    choice_detail: GRUVBOX_LIGHT.muted,
    modal_footer: GRUVBOX_LIGHT.muted,
    button_accent: GRUVBOX_LIGHT.yellow,
    settings_error: GRUVBOX_LIGHT.yellow,
    settings_info: GRUVBOX_LIGHT.muted,
    settings_readonly: GRUVBOX_LIGHT.muted,
    settings_help: GRUVBOX_LIGHT.muted,
    scrollbar_thumb: GRUVBOX_LIGHT.blue,
    scrollbar_rail: GRUVBOX_LIGHT.muted,
    rail_done: GRUVBOX_LIGHT.green,
    rail_active: GRUVBOX_LIGHT.blue,
    rail_muted: GRUVBOX_LIGHT.muted,
    rail_connector: GRUVBOX_LIGHT.muted,
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

/// The chrome table of a non-default theme key (T109.1).
fn chrome_of(theme: ThemeKey) -> &'static Chrome {
    match theme {
        ThemeKey::AtomOneDark => &ATOM_ONE_DARK_CHROME,
        ThemeKey::AtomOneLight => &ATOM_ONE_LIGHT_CHROME,
        ThemeKey::TokyoNightDark => &TOKYO_NIGHT_DARK_CHROME,
        ThemeKey::TokyoNightDay => &TOKYO_NIGHT_DAY_CHROME,
        ThemeKey::CatppuccinMocha => &CATPPUCCIN_MOCHA_CHROME,
        ThemeKey::CatppuccinLatte => &CATPPUCCIN_LATTE_CHROME,
        ThemeKey::SolarizedDark => &SOLARIZED_DARK_CHROME,
        ThemeKey::SolarizedLight => &SOLARIZED_LIGHT_CHROME,
        ThemeKey::GruvboxDark => &GRUVBOX_DARK_CHROME,
        ThemeKey::GruvboxLight => &GRUVBOX_LIGHT_CHROME,
        ThemeKey::Dark => unreachable!("the default theme is Theme::DARK, not a palette"),
    }
}

impl Palette {
    /// Emits the theme: the palette's own [`Chrome`] table (T109.1) supplies
    /// every non-agent-text semantic field, and the palette's background fixes
    /// the agent line colours (T108.1). The truecolor setting picks the
    /// emission, exactly like the agent text roles.
    fn theme(&self, chrome: &Chrome, truecolor: bool) -> Theme {
        let colour = |role: (u8, u8, u8)| {
            if truecolor {
                Color::Rgb(role.0, role.1, role.2)
            } else {
                Color::Indexed(rgb_to_indexed(role))
            }
        };
        Theme {
            background: colour(chrome.background),
            foreground: colour(chrome.foreground),
            chip_stopped: colour(chrome.chip_stopped),
            chip_running: colour(chrome.chip_running),
            chip_planning: colour(chrome.chip_planning),
            chip_discovering: colour(chrome.chip_discovering),
            chip_stopping: colour(chrome.chip_stopping),
            chip_text: colour(chrome.chip_text),
            run_mode_chip: colour(chrome.run_mode_chip),
            status_message: colour(chrome.status_message),
            status_key: colour(chrome.status_key),
            status_label: colour(chrome.status_label),
            agent_text: AgentText::resolve(self.background, truecolor),
            agent_names: AgentNames::resolve(self.background, truecolor),
            frame_title_detail: colour(chrome.frame_title_detail),
            pane_empty: colour(chrome.pane_empty),
            task_done: colour(chrome.task_done),
            task_running: colour(chrome.task_running),
            task_new: colour(chrome.task_new),
            tasks_empty: colour(chrome.tasks_empty),
            dialog_status: colour(chrome.dialog_status),
            cursor_foreground: colour(chrome.cursor_foreground),
            cursor_background: colour(chrome.cursor_background),
            dialog_hint: colour(chrome.dialog_hint),
            choice_detail: colour(chrome.choice_detail),
            modal_footer: colour(chrome.modal_footer),
            button_accent: colour(chrome.button_accent),
            settings_error: colour(chrome.settings_error),
            settings_info: colour(chrome.settings_info),
            settings_readonly: colour(chrome.settings_readonly),
            settings_help: colour(chrome.settings_help),
            scrollbar_thumb: colour(chrome.scrollbar_thumb),
            scrollbar_rail: colour(chrome.scrollbar_rail),
            rail_done: colour(chrome.rail_done),
            rail_active: colour(chrome.rail_active),
            rail_muted: colour(chrome.rail_muted),
            rail_connector: colour(chrome.rail_connector),
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
        assert_eq!(dark.agent_names.planner, Color::Magenta);
        assert_eq!(dark.agent_names.builder, Color::Green);
        assert_eq!(dark.agent_names.reviewer, Color::LightYellow);
        assert_eq!(dark.agent_names.research, Color::LightBlue);
        assert_eq!(dark.agent_names.discovery, Color::Cyan);
        assert_eq!(dark.agent_names.orchestrator, Color::LightMagenta);
        assert_eq!(dark.agent_names.other, Color::DarkGray);
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
            theme.agent_names.planner,
            theme.agent_names.builder,
            theme.agent_names.reviewer,
            theme.agent_names.research,
            theme.agent_names.discovery,
            theme.agent_names.orchestrator,
            theme.agent_names.other,
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

    /// Every agent keeps the same name colour identity in every built-in
    /// theme (T110.1): the default stays on its value-locked named ANSI
    /// colours, and every palette emits each agent's canonical anchor hue —
    /// planner magenta, builder green, reviewer orange, research blue,
    /// discovery cyan, orchestrator rose — with only the anchor's lightness
    /// adjusted to the palette's background. Any other agent name stays the
    /// near-neutral gray. Identity is asserted in truecolor mode; the
    /// 256-colour mode derives from the same RGB.
    #[test]
    fn agent_name_identity_is_fixed_across_themes() {
        let anchors = [
            ("planner", PLANNER_NAME_ANCHOR),
            ("builder", BUILDER_NAME_ANCHOR),
            ("reviewer", REVIEWER_NAME_ANCHOR),
            ("research", RESEARCH_NAME_ANCHOR),
            ("discovery", DISCOVERY_NAME_ANCHOR),
            ("orchestrator", ORCHESTRATOR_NAME_ANCHOR),
        ];
        for key in every_key() {
            let theme = Theme::resolve(key, Some(true));
            if key == ThemeKey::Dark {
                assert_eq!(Theme::agent_name_color(theme, "planner"), Color::Magenta);
                assert_eq!(Theme::agent_name_color(theme, "builder"), Color::Green);
                assert_eq!(
                    Theme::agent_name_color(theme, "reviewer"),
                    Color::LightYellow
                );
                assert_eq!(Theme::agent_name_color(theme, "research"), Color::LightBlue);
                assert_eq!(Theme::agent_name_color(theme, "discovery"), Color::Cyan);
                assert_eq!(
                    Theme::agent_name_color(theme, "orchestrator"),
                    Color::LightMagenta
                );
                assert_eq!(
                    Theme::agent_name_color(theme, "something-new"),
                    Color::DarkGray
                );
                continue;
            }
            // The tolerance covers byte-rounding quantization, not a real
            // remapping: the anchor hues sit more than thirty degrees apart.
            for (name, anchor) in anchors {
                let Color::Rgb(r, g, b) = Theme::agent_name_color(theme, name) else {
                    panic!("{key:?} must emit RGB in truecolor mode");
                };
                let hue = rgb_to_hsl((r, g, b)).0;
                let anchor_hue = rgb_to_hsl(anchor).0;
                let drift = (hue - anchor_hue)
                    .abs()
                    .min(360.0 - (hue - anchor_hue).abs());
                assert!(
                    drift <= 2.0,
                    "{key:?} agent {name} drifted {drift:.1} degrees off its anchor hue"
                );
            }
            let Color::Rgb(r, g, b) = Theme::agent_name_color(theme, "something-new") else {
                panic!("{key:?} must emit RGB in truecolor mode");
            };
            let spread = r.max(g).max(b) - r.min(g).min(b);
            assert!(
                spread <= 8,
                "{key:?} unknown agent must stay a neutral gray, got ({r}, {g}, {b})"
            );
        }
    }

    /// Every agent name colour stays readable and distinct on its theme's
    /// background (T110.1): after the palette's lightness adjustment each of
    /// the seven identities differs from the background, reaches the WCAG
    /// contrast threshold, is pairwise distinct as a value, and the six
    /// chromatic agents keep at least fifteen degrees of pairwise hue
    /// separation, so no two agents can be confused on any theme. The
    /// default theme is excluded because its background is the terminal
    /// default (`Color::Reset`); its identity is value-locked by the DARK
    /// test above. Asserted in truecolor mode.
    #[test]
    fn agent_names_are_readable_and_distinct_on_every_theme_background() {
        let names = [
            "planner",
            "builder",
            "reviewer",
            "research",
            "discovery",
            "orchestrator",
            "something-new",
        ];
        for key in every_key().into_iter().filter(|key| *key != ThemeKey::Dark) {
            let theme = Theme::resolve(key, Some(true));
            let Color::Rgb(r, g, b) = theme.background else {
                panic!("{key:?} palettes always emit RGB backgrounds");
            };
            let background = (r, g, b);
            let mut resolved = Vec::new();
            for name in names {
                let Color::Rgb(r, g, b) = Theme::agent_name_color(theme, name) else {
                    panic!("{key:?} must emit RGB in truecolor mode");
                };
                let colour = (r, g, b);
                assert_ne!(
                    colour, background,
                    "{key:?} agent {name} must differ from the theme background"
                );
                let ratio = contrast_ratio(colour, background);
                assert!(
                    ratio >= READABLE_CONTRAST,
                    "{key:?} agent {name} reaches only {ratio} against the background"
                );
                resolved.push(colour);
            }
            for (i, left) in resolved.iter().enumerate() {
                for right in &resolved[i + 1..] {
                    assert_ne!(
                        left, right,
                        "{key:?} two agent name colours resolved identically"
                    );
                }
            }
            // The chromatic identities: circular hue distance, never a
            // remapping's near-miss.
            let hues: Vec<f64> = resolved[..6]
                .iter()
                .map(|&(r, g, b)| rgb_to_hsl((r, g, b)).0)
                .collect();
            for (i, &left) in hues.iter().enumerate() {
                for &right in &hues[i + 1..] {
                    let distance = (left - right).abs().min(360.0 - (left - right).abs());
                    assert!(
                        distance >= 15.0,
                        "{key:?} two agent name hues sit only {distance:.1} degrees apart"
                    );
                }
            }
        }
    }

    /// Every chrome semantic field of a theme, in a fixed order matching the
    /// [`Chrome`] struct, for the per-theme table checks.
    fn every_chrome_field(theme: &Theme) -> Vec<Color> {
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

    /// Every non-fixated semantic field resolves from the theme's own
    /// [`Chrome`] table (T109.1): a theme that silently fell back to a shared
    /// mapping rule would fail this. A field added to `Theme` without a
    /// `Chrome` counterpart fails to compile. The agent text and agent name
    /// fields are asserted separately above (T108.1, T110.1). Checked in
    /// truecolor mode; the 256-colour mode derives from the same triples.
    #[test]
    fn chrome_mapping_is_per_theme() {
        for key in every_key().into_iter().filter(|key| *key != ThemeKey::Dark) {
            let theme = Theme::resolve(key, Some(true));
            let chrome = chrome_of(key);
            let expected = [
                chrome.background,
                chrome.foreground,
                chrome.chip_stopped,
                chrome.chip_running,
                chrome.chip_planning,
                chrome.chip_discovering,
                chrome.chip_stopping,
                chrome.chip_text,
                chrome.run_mode_chip,
                chrome.status_message,
                chrome.status_key,
                chrome.status_label,
                chrome.frame_title_detail,
                chrome.pane_empty,
                chrome.task_done,
                chrome.task_running,
                chrome.task_new,
                chrome.tasks_empty,
                chrome.dialog_status,
                chrome.cursor_foreground,
                chrome.cursor_background,
                chrome.dialog_hint,
                chrome.choice_detail,
                chrome.modal_footer,
                chrome.button_accent,
                chrome.settings_error,
                chrome.settings_info,
                chrome.settings_readonly,
                chrome.settings_help,
                chrome.scrollbar_thumb,
                chrome.scrollbar_rail,
                chrome.rail_done,
                chrome.rail_active,
                chrome.rail_muted,
                chrome.rail_connector,
            ];
            for (got, want) in every_chrome_field(&theme).into_iter().zip(expected) {
                assert_eq!(
                    got,
                    Color::Rgb(want.0, want.1, want.2),
                    "{key:?} chrome field must resolve from its per-theme table"
                );
            }
        }
    }

    /// The one chip text reads on every chip background of every theme except
    /// the benchmark: after the per-theme retune (T109.1) each of the seven
    /// chip backgrounds reaches [`READABLE_CONTRAST`] under the shared
    /// `chip_text`. Tokyo Night Dark is exempt because its look is the
    /// snapshot-locked benchmark the other themes are tuned to reach.
    #[test]
    fn chip_text_reads_on_every_chip_background() {
        for key in every_key()
            .into_iter()
            .filter(|key| !matches!(key, ThemeKey::Dark | ThemeKey::TokyoNightDark))
        {
            let chrome = chrome_of(key);
            for chip in [
                chrome.chip_stopped,
                chrome.chip_running,
                chrome.chip_planning,
                chrome.chip_discovering,
                chrome.chip_stopping,
                chrome.run_mode_chip,
                chrome.status_key,
            ] {
                let ratio = contrast_ratio(chrome.chip_text, chip);
                assert!(
                    ratio >= READABLE_CONTRAST,
                    "{key:?} chip text reaches only {ratio} on a chip background"
                );
            }
        }
    }

    /// The block cursor's foreground reads on its background in every theme
    /// except the benchmark (the same snapshot-locked exemption as the chip
    /// test above).
    #[test]
    fn cursor_foreground_reads_on_cursor_background() {
        for key in every_key()
            .into_iter()
            .filter(|key| !matches!(key, ThemeKey::Dark | ThemeKey::TokyoNightDark))
        {
            let chrome = chrome_of(key);
            let ratio = contrast_ratio(chrome.cursor_foreground, chrome.cursor_background);
            assert!(
                ratio >= READABLE_CONTRAST,
                "{key:?} cursor foreground reaches only {ratio} on the cursor background"
            );
        }
    }

    /// Every light theme's chrome text reads on its background (T109.1): all
    /// text-bearing fields reach [`READABLE_CONTRAST`] and the decorative
    /// scrollbar fields merely differ from it. Scoped to the light variants
    /// deliberately: dim muted text is an accepted aesthetic on the dark
    /// themes, where the benchmark theme itself sits near the threshold.
    #[test]
    fn light_theme_chrome_text_is_readable() {
        for key in [
            ThemeKey::AtomOneLight,
            ThemeKey::TokyoNightDay,
            ThemeKey::CatppuccinLatte,
            ThemeKey::SolarizedLight,
            ThemeKey::GruvboxLight,
        ] {
            let chrome = chrome_of(key);
            let text_fields = [
                ("foreground", chrome.foreground),
                ("status_label", chrome.status_label),
                ("status_message", chrome.status_message),
                ("frame_title_detail", chrome.frame_title_detail),
                ("pane_empty", chrome.pane_empty),
                ("task_done", chrome.task_done),
                ("task_running", chrome.task_running),
                ("task_new", chrome.task_new),
                ("tasks_empty", chrome.tasks_empty),
                ("dialog_status", chrome.dialog_status),
                ("dialog_hint", chrome.dialog_hint),
                ("choice_detail", chrome.choice_detail),
                ("modal_footer", chrome.modal_footer),
                ("button_accent", chrome.button_accent),
                ("settings_error", chrome.settings_error),
                ("settings_info", chrome.settings_info),
                ("settings_readonly", chrome.settings_readonly),
                ("settings_help", chrome.settings_help),
                ("rail_done", chrome.rail_done),
                ("rail_active", chrome.rail_active),
                ("rail_muted", chrome.rail_muted),
                ("rail_connector", chrome.rail_connector),
            ];
            for (field, text) in text_fields {
                let ratio = contrast_ratio(text, chrome.background);
                assert!(
                    ratio >= READABLE_CONTRAST,
                    "{key:?} {field} reaches only {ratio} on the background"
                );
            }
            assert_ne!(chrome.scrollbar_thumb, chrome.background);
            assert_ne!(chrome.scrollbar_rail, chrome.background);
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
