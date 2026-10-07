//! The colour theme (T34.1, T111.1): the single source of every colour the
//! shell renders, as a class-based system. The renderers use the five basic
//! classes `background`, `foreground`, `normal-text`, `muted-text` and
//! `highlighted-text`, plus only the special classes the basics cannot
//! express. No code outside this module constructs a `ratatui::style::Color`
//! literal. Classes are shared: every use of one kind of text sits on the
//! same field, so two uses are not independently restyleable — the deliberate
//! reversal of the pre-T111.1 one-field-per-use design. Renderers reach the
//! theme through [`crate::app::App::theme`], which resolves it from the
//! shell's tui settings, so a config reload picks up changes without extra
//! plumbing. Markdown rendering takes its colours from the base style
//! (`thinking`), so it needs no fields of its own.
//!
//! Eleven built-in variants: the default `dark` plus ten published palettes
//! (Atom One, Tokyo Night, Catppuccin, Solarized and Gruvbox, each in a dark and
//! a light variant). Every non-default variant is defined once as a
//! [`Palette`] of signature roles and carries its own [`Classes`] table that
//! maps those roles onto the classes (T111.1), so each theme tunes its
//! classes — chips, cursor, muted text — without touching the others, and every
//! renderer works unchanged with any variant. Since T112.1 the tables are
//! genuinely per-theme: no two themes share a single class value, and every
//! palette's text classes clear the readability threshold on its own
//! background, so switching themes changes the whole app look. Since T119.1
//! `highlighted_text` is each theme's own signature accent instead of the
//! shared yellow convention the tables first used; the default dark theme
//! keeps its value-locked yellow. RGB colours
//! emit as `Color::Rgb` while the truecolor setting is on and as the nearest
//! xterm-256 indexed colour while it is off.
//!
//! The agent output line colours live in their own [`AgentText`] subclass
//! (T108.1), structurally separate from the classes so the output identity
//! stays stable across theme refactoring. Each line kind's colour
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
//! never remaps onto a palette role. The mapping feeds only the output frame
//! title's agent type name; every other agent-name occurrence — the lifecycle
//! status lines, the planning heading, the status bar's run message and the
//! headless `--color` output — wears the surrounding text's colour (T117.1).
//!
//! Class-to-use map (all sites are in `ui.rs` unless noted):
//!
//! | Class | Renderer | Use |
//! |---|---|---|
//! | `background` | `render` | the frame's base background, painted first |
//! | `foreground` | `render` | the frame's base foreground, inherited by unstyled text |
//! | `normal_text` | `button_line`, `theme_row_line` | the modal buttons' labels (T64.1), the theme picker's unselected entry names (T115.1) |
//! | `muted_text` | `render_modal_footer`, `render_hints_strip`, `status_widget`, `render_output`, `render_pane`, `render_tasks`, `render_dialog`, `render_stop_dialog`, `render_settings_confirm`, `render_settings_overlay`, `settings_row_line`, `render_settings_help`, `pipeline::tile_style`, `pipeline::render_rail` | every low-emphasis text: modal footers and the focused frame's hints strip (T59.1, T71.1), the status bar's key-chip labels, the output frame title's separator, provider, model and timer and everything after the word `Tasks` in the tasks frame title (T37.1, T67.1), the "no output yet" and "no tasks in TASKS.md" placeholders, done task rows, the empty-input watermark (T41.1), "-- detail" choice text on unselected rows, the settings overlay's info, unselected read-only and help text (T85.1), the rail's muted and pending tiles, the provider/model detail and the down-arrow connectors |
//! | `highlighted_text` | `status_widget`, `render_tasks`, `render_dialog`, `button_line`, `render_settings_overlay`, `settings_row_line`, `render_settings_confirm`, `render_stop_dialog`, `theme_row_line`, `render_scrollbar` | the status bar's message, the STOPPING chip, new tasks, the add-task dialog's status line, every modal's " [ Key ] " markers, the settings overlay's error line, and the selected row of every selection-bearing modal (T118.1): the theme picker's selected entry (T115.1), the settings overlay's focused row, the unsaved-changes and stop dialogs' selected choices, and the overlay scrollbar thumb (T121.1) |
//! | `chip_neutral` | `status_widget` | STOPPED, run-mode and key-chip backgrounds |
//! | `contrast_text` | `status_widget`, `render_dialog` | foreground on every status chip and the dialog input's block cursor |
//! | `success` | `status_widget`, `render_tasks`, `pipeline::tile_style` | the RUNNING chip, running task rows, done rail tiles (T50.1) and SHIP while shipping |
//! | `chip_planning` | `status_widget` | PLANNING status chip background |
//! | `chip_discovering` | `status_widget` | DISCOVERING status chip background |
//! | `accent` | `render_dialog`, `pipeline::tile_style` | the dialog input's block cursor background, the active rail tile and DISCOVER while a round runs |
//! | `scrollbar_rail` | `render_scrollbar` | the overlay scrollbar's rail |
//! | `agent_text.thinking` | `style_of`, headless `--color` | agent thinking; also the markdown base style |
//! | `agent_text.tool` | `style_of`, headless `--color` | tool-call lines |
//! | `agent_text.result` | `style_of`, headless `--color` | result lines |
//! | `agent_text.error` | `style_of`, headless `--color` | error lines |
//! | `agent_text.notice` | `style_of`, headless `--color` | notice lines |
//! | `agent_text.heading` | `style_of`, headless `--color` | markdown headings (bold added by the renderer) |
//! | `agent_names.*` | `render_output` | the output frame title's agent type name only (T110.1, T117.1) |
//! | `agent_text.pane_status` | `style_of`, headless `--color` | the agent lifecycle status lines in the output pane (T78.1, T42.1), the agent name included (T117.1) |

use crate::app::LineKind;
use patok_core::config::Theme as ThemeKey;
use ratatui::style::Color;

/// The agent output line colours (T108.1): structurally separate from the
/// classes so the output identity stays stable across theme refactoring. Each kind's hue is fixed for every built-in theme; a palette
/// only adjusts lightness so the fixed hue stays readable on its background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentText {
    /// Agent thinking lines; the markdown base style.
    pub thinking: Color,
    /// Tool-call lines.
    pub tool: Color,
    /// Result lines: a muted gray, independent of the `muted_text` class.
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
    /// picks the emission, exactly like the classes.
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

/// The shell's colour theme (T111.1): a class-based system. The five basic
/// classes cover every text and surface use; the special classes exist only
/// for colours the basics cannot express. Unlike the pre-T111.1 design of one
/// named semantic field per colour use, classes are shared: every use of one
/// kind of text sits on the same field, so two uses are not independently
/// restyleable. The agent output line colours (T108.1) and agent name colours
/// (T110.1) stay fixated in their own subclasses, outside the class system.
/// `Copy`, so renderers pass it around freely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// The `background` basic class: the shell's base background, painted
    /// over the whole frame first.
    pub background: Color,
    /// The `foreground` basic class: the shell's base foreground, inherited by
    /// every unstyled text cell.
    pub foreground: Color,
    /// The `normal-text` basic class: explicitly styled plain text, such as
    /// the modal buttons' labels (T64.1).
    pub normal_text: Color,
    /// The `muted-text` basic class: every low-emphasis text and connector.
    pub muted_text: Color,
    /// The `highlighted-text` basic class: every emphasized text, the
    /// STOPPING chip background and the overlay scrollbar thumb (T121.1).
    pub highlighted_text: Color,
    /// The agent output line colours (T108.1), held apart from the classes so
    /// the output identity stays stable across theme refactoring.
    pub agent_text: AgentText,
    /// The agent name colours (T110.1): one fixed identity per agent, the
    /// same hue family in every built-in theme.
    pub agent_names: AgentNames,
    /// The neutral-chip special class: STOPPED, run-mode and key-chip
    /// backgrounds. Special because it is a chip background the contrast
    /// text must read on, not a text class: each theme's table sits it
    /// wherever that holds (typically the foreground role).
    pub chip_neutral: Color,
    /// The contrast-text special class: the foreground on the saturated chip
    /// and cursor backgrounds. Special because the default theme's value is
    /// `Black` while its background is `Reset`.
    pub contrast_text: Color,
    /// The success special class: the RUNNING chip, running task rows and
    /// done rail tiles. A saturated green no basic class expresses.
    pub success: Color,
    /// The PLANNING status chip background: a saturated magenta no basic class
    /// expresses.
    pub chip_planning: Color,
    /// The DISCOVERING status chip background: a saturated blue no basic class
    /// expresses (and distinct from `accent` in every theme).
    pub chip_discovering: Color,
    /// The accent special class: the active rail tile and the dialog cursor
    /// background. A saturated cyan/blue no basic class expresses.
    pub accent: Color,
    /// The scrollbar-rail special class: the overlay scrollbar's rail,
    /// deliberately dimmer than `muted_text` on some themes.
    pub scrollbar_rail: Color,
}

impl Theme {
    /// The default dark theme: exactly the look the shell had before the class
    /// system existed, value-locked by the unit tests below.
    pub const DARK: Theme = Theme {
        background: Color::Reset,
        foreground: Color::Reset,
        normal_text: Color::Reset,
        muted_text: Color::DarkGray,
        highlighted_text: Color::Yellow,
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
        chip_neutral: Color::DarkGray,
        contrast_text: Color::Black,
        success: Color::Green,
        chip_planning: Color::Magenta,
        chip_discovering: Color::Blue,
        accent: Color::Cyan,
        scrollbar_rail: Color::DarkGray,
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
                classes_of(other),
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
    /// Used only by the output frame title's agent type name (T117.1).
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
/// has its own [`Classes`] table ([`Palette::theme`]) that maps the roles onto
/// the colour classes, which is what keeps every renderer variant-agnostic.
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
    /// The palette's accent.
    blue: (u8, u8, u8),
    magenta: (u8, u8, u8),
}

/// One theme's class table (T111.1): the raw RGB triple every class of
/// [`Theme`] resolves to, in the same order as the `Theme` fields minus the
/// fixated subclasses. Each built-in palette owns one table — there is no
/// shared mapping rule — so a theme can tune any class without touching the
/// others. The agent line colours and agent name colours are fixated
/// separately by [`AgentText`] (T108.1) and [`AgentNames`] (T110.1) and never
/// come from here.
struct Classes {
    /// The `background` basic class: the theme's base background.
    background: (u8, u8, u8),
    /// The `foreground` basic class: the theme's base foreground.
    foreground: (u8, u8, u8),
    /// The `normal-text` basic class: explicitly styled plain text.
    normal_text: (u8, u8, u8),
    /// The `muted-text` basic class: every low-emphasis text and connector.
    muted_text: (u8, u8, u8),
    /// The `highlighted-text` basic class: every emphasized text, the
    /// STOPPING chip background and the overlay scrollbar thumb (T121.1).
    highlighted_text: (u8, u8, u8),
    /// The neutral-chip special class: STOPPED, run-mode and key-chip
    /// backgrounds.
    chip_neutral: (u8, u8, u8),
    /// The contrast-text special class: the foreground on the saturated chip
    /// and cursor backgrounds.
    contrast_text: (u8, u8, u8),
    /// The success special class: RUNNING chip, running tasks, done rail
    /// tiles.
    success: (u8, u8, u8),
    /// The PLANNING status chip background.
    chip_planning: (u8, u8, u8),
    /// The DISCOVERING status chip background.
    chip_discovering: (u8, u8, u8),
    /// The accent special class: active rail tile, cursor background.
    accent: (u8, u8, u8),
    /// The scrollbar-rail special class: the overlay scrollbar's rail.
    scrollbar_rail: (u8, u8, u8),
}

const ATOM_ONE_DARK: Palette = Palette {
    background: (0x28, 0x2C, 0x34),
    foreground: (0xAB, 0xB2, 0xBF),
    muted: (0x5C, 0x63, 0x70),
    surface: (0x3E, 0x44, 0x51),
    green: (0x98, 0xC3, 0x79),
    blue: (0x61, 0xAF, 0xEF),
    magenta: (0xC6, 0x78, 0xDD),
};

/// The neutral chips (stopped, run-mode, key) sit `chip_neutral` on the
/// foreground role: the background-role contrast text only reads on chip
/// backgrounds at least as bright as the accents, and the surface role is
/// not. The palette's muted (2.3:1 on the background) is too dim for the
/// readability bar, so `muted_text` takes a lightened muted literal that
/// clears [`READABLE_CONTRAST`]. `normal_text` takes a lightened foreground
/// literal and `accent` a lightened blue literal — brighter than the
/// DISCOVERING chip — so the theme's classes are its own (T112.1); the
/// decorative scrollbar rail keeps the palette value. `highlighted_text`
/// takes a magenta literal of the planner-name family (T120.1) — between
/// the planner anchor `B7 6B D8` and the palette's own magenta, so the
/// highlight echoes the PLANNING identity without duplicating the PLANNING
/// chip — and it clears [`READABLE_CONTRAST`] on the background. It stays
/// unique among the eleven built-in themes' highlights (T119.1): apart
/// from Atom One Light's darkened magenta and the default dark theme's
/// value-locked yellow.
const ATOM_ONE_DARK_CLASSES: Classes = Classes {
    background: ATOM_ONE_DARK.background,
    foreground: ATOM_ONE_DARK.foreground,
    normal_text: (0xC8, 0xCD, 0xD5),
    muted_text: (0x6E, 0x77, 0x86),
    highlighted_text: (0xBF, 0x72, 0xE0),
    chip_neutral: ATOM_ONE_DARK.foreground,
    contrast_text: ATOM_ONE_DARK.background,
    success: ATOM_ONE_DARK.green,
    chip_planning: ATOM_ONE_DARK.magenta,
    chip_discovering: ATOM_ONE_DARK.blue,
    accent: (0x8B, 0xC4, 0xF3),
    scrollbar_rail: ATOM_ONE_DARK.muted,
};

const ATOM_ONE_LIGHT: Palette = Palette {
    background: (0xFA, 0xFA, 0xFA),
    foreground: (0x38, 0x3A, 0x42),
    muted: (0xA0, 0xA1, 0xA7),
    surface: (0xE5, 0xE5, 0xE5),
    green: (0x50, 0xA1, 0x4F),
    blue: (0x40, 0x78, 0xF2),
    magenta: (0xA6, 0x26, 0xA4),
};

/// Light theme: `chip_neutral` takes the foreground role so the
/// background-role contrast text reads on it, and `muted_text` takes a
/// darkened muted literal that clears [`READABLE_CONTRAST`] on the
/// near-white background. `normal_text` takes a darkened foreground literal
/// and `accent` a darkened blue literal — deeper than the DISCOVERING
/// chip — so the theme's classes are its own (T112.1). `highlighted_text`
/// takes a darkened literal of the palette's magenta hue, deeper than the
/// PLANNING chip, so the theme's highlight is its own (T119.1).
const ATOM_ONE_LIGHT_CLASSES: Classes = Classes {
    background: ATOM_ONE_LIGHT.background,
    foreground: ATOM_ONE_LIGHT.foreground,
    normal_text: (0x2C, 0x2E, 0x34),
    muted_text: (0x86, 0x87, 0x8E),
    highlighted_text: (0x8B, 0x1F, 0x89),
    chip_neutral: ATOM_ONE_LIGHT.foreground,
    contrast_text: ATOM_ONE_LIGHT.background,
    success: ATOM_ONE_LIGHT.green,
    chip_planning: ATOM_ONE_LIGHT.magenta,
    chip_discovering: ATOM_ONE_LIGHT.blue,
    accent: (0x1F, 0x60, 0xF0),
    scrollbar_rail: ATOM_ONE_LIGHT.muted,
};

const TOKYO_NIGHT_DARK: Palette = Palette {
    background: (0x1A, 0x1B, 0x26),
    foreground: (0xC0, 0xCA, 0xF5),
    muted: (0x56, 0x5F, 0x89),
    surface: (0x33, 0x46, 0x7C),
    green: (0x9E, 0xCE, 0x6A),
    blue: (0x7A, 0xA2, 0xF7),
    magenta: (0xBB, 0x9A, 0xF7),
};

/// The T109.1 snapshot lock was released in T112.1: the table tunes
/// readability and the theme's own look like every other palette. The
/// palette's muted (2.8:1 on the background) is too dim for the readability
/// bar, so `muted_text` takes a lightened muted literal that clears
/// [`READABLE_CONTRAST`]; `chip_neutral` moves from the palette's dim
/// `surface` role to the foreground role so the background-role contrast
/// text reads on the neutral chips; the scrollbar rail takes the freed
/// `surface` role; and `accent` takes a lightened blue literal, brighter
/// than the DISCOVERING chip. `highlighted_text` takes Tokyo Night's
/// signature cyan, brighter than the blue chip family, so the theme's
/// highlight is its own (T119.1).
const TOKYO_NIGHT_DARK_CLASSES: Classes = Classes {
    background: TOKYO_NIGHT_DARK.background,
    foreground: TOKYO_NIGHT_DARK.foreground,
    normal_text: TOKYO_NIGHT_DARK.foreground,
    muted_text: (0x65, 0x6F, 0x9E),
    highlighted_text: (0x7D, 0xCF, 0xFF),
    chip_neutral: TOKYO_NIGHT_DARK.foreground,
    contrast_text: TOKYO_NIGHT_DARK.background,
    success: TOKYO_NIGHT_DARK.green,
    chip_planning: TOKYO_NIGHT_DARK.magenta,
    chip_discovering: TOKYO_NIGHT_DARK.blue,
    accent: (0xA5, 0xC0, 0xFA),
    scrollbar_rail: TOKYO_NIGHT_DARK.surface,
};

const TOKYO_NIGHT_DAY: Palette = Palette {
    background: (0xE1, 0xE2, 0xE7),
    foreground: (0x37, 0x60, 0xBF),
    muted: (0x84, 0x8C, 0xB5),
    surface: (0xC4, 0xC8, 0xDA),
    green: (0x58, 0x75, 0x39),
    blue: (0x2E, 0x7D, 0xE9),
    magenta: (0x98, 0x54, 0xF1),
};

/// Light theme: `chip_neutral` takes the foreground role so the
/// background-role contrast text reads on it, and `muted_text` takes a
/// darkened muted literal that clears [`READABLE_CONTRAST`].
/// `normal_text` takes a darkened foreground literal and `accent` a darkened
/// blue literal — deeper than the DISCOVERING chip — so the theme's classes
/// are its own (T112.1). `highlighted_text` takes a darkened literal of the
/// palette's magenta hue, deeper than the PLANNING chip, so the theme's
/// highlight is its own (T119.1).
const TOKYO_NIGHT_DAY_CLASSES: Classes = Classes {
    background: TOKYO_NIGHT_DAY.background,
    foreground: TOKYO_NIGHT_DAY.foreground,
    normal_text: (0x31, 0x56, 0xAB),
    muted_text: (0x71, 0x7A, 0xAA),
    highlighted_text: (0x7A, 0x3A, 0xC4),
    chip_neutral: TOKYO_NIGHT_DAY.foreground,
    contrast_text: TOKYO_NIGHT_DAY.background,
    success: TOKYO_NIGHT_DAY.green,
    chip_planning: TOKYO_NIGHT_DAY.magenta,
    chip_discovering: TOKYO_NIGHT_DAY.blue,
    accent: (0x18, 0x6D, 0xE1),
    scrollbar_rail: TOKYO_NIGHT_DAY.muted,
};

const CATPPUCCIN_MOCHA: Palette = Palette {
    background: (0x1E, 0x1E, 0x2E),
    foreground: (0xCD, 0xD6, 0xF4),
    muted: (0x7F, 0x84, 0x9C),
    surface: (0x31, 0x32, 0x44),
    green: (0xA6, 0xE3, 0xA1),
    blue: (0x89, 0xB4, 0xFA),
    magenta: (0xCB, 0xA6, 0xF7),
};

/// Mocha's own muted and accents already clear [`READABLE_CONTRAST`] on its
/// background, so `chip_neutral` keeps the foreground role and only
/// `normal_text` takes a lightened foreground literal and `accent` a
/// lightened blue literal — brighter than the DISCOVERING chip — to make
/// the theme's classes its own (T112.1). `highlighted_text` takes
/// Catppuccin's pink, a signature Mocha accent no other class uses, so the
/// theme's highlight is its own (T119.1).
const CATPPUCCIN_MOCHA_CLASSES: Classes = Classes {
    background: CATPPUCCIN_MOCHA.background,
    foreground: CATPPUCCIN_MOCHA.foreground,
    normal_text: (0xEA, 0xEE, 0xFA),
    muted_text: CATPPUCCIN_MOCHA.muted,
    highlighted_text: (0xF5, 0xC2, 0xE8),
    chip_neutral: CATPPUCCIN_MOCHA.foreground,
    contrast_text: CATPPUCCIN_MOCHA.background,
    success: CATPPUCCIN_MOCHA.green,
    chip_planning: CATPPUCCIN_MOCHA.magenta,
    chip_discovering: CATPPUCCIN_MOCHA.blue,
    accent: (0xB5, 0xD0, 0xFC),
    scrollbar_rail: CATPPUCCIN_MOCHA.muted,
};

const CATPPUCCIN_LATTE: Palette = Palette {
    background: (0xEF, 0xF1, 0xF5),
    foreground: (0x4C, 0x4F, 0x69),
    muted: (0x7C, 0x7F, 0x94),
    surface: (0xCC, 0xD0, 0xDA),
    green: (0x40, 0xA0, 0x2B),
    blue: (0x1E, 0x66, 0xF5),
    magenta: (0x88, 0x39, 0xEF),
};

/// Light theme: `chip_neutral` takes the foreground role so the
/// background-role contrast text reads on it. Latte's muted already clears
/// [`READABLE_CONTRAST`], but its green falls short both as a chip
/// background under the contrast text and as text on the background, so
/// `success` takes a darkened literal of the role's hue;
/// `highlighted_text` takes a darkened literal of the palette's pink, deeper
/// than the PLANNING chip, so the theme's highlight is its own (T119.1).
/// `accent` takes a darkened blue literal, deeper than the
/// DISCOVERING chip's raw blue (T112.1).
const CATPPUCCIN_LATTE_CLASSES: Classes = Classes {
    background: CATPPUCCIN_LATTE.background,
    foreground: CATPPUCCIN_LATTE.foreground,
    normal_text: CATPPUCCIN_LATTE.foreground,
    muted_text: CATPPUCCIN_LATTE.muted,
    highlighted_text: (0xC0, 0x4F, 0xA6),
    chip_neutral: CATPPUCCIN_LATTE.foreground,
    contrast_text: CATPPUCCIN_LATTE.background,
    success: (0x3D, 0x98, 0x29),
    chip_planning: CATPPUCCIN_LATTE.magenta,
    chip_discovering: CATPPUCCIN_LATTE.blue,
    accent: (0x0A, 0x52, 0xE0),
    scrollbar_rail: CATPPUCCIN_LATTE.muted,
};

const SOLARIZED_DARK: Palette = Palette {
    background: (0x00, 0x2B, 0x36),
    foreground: (0x83, 0x94, 0x96),
    muted: (0x58, 0x6E, 0x75),
    surface: (0x07, 0x36, 0x42),
    green: (0x85, 0x99, 0x00),
    blue: (0x26, 0x8B, 0xD2),
    magenta: (0xD3, 0x36, 0x82),
};

/// Solarized Dark's muted (2.8:1 on the background) is too dim for the
/// readability bar, so `muted_text` takes a lightened muted literal that
/// clears [`READABLE_CONTRAST`]. The Solarized twins share their raw
/// green/yellow/blue/magenta roles, so this table takes brightened literals
/// of the magenta and blue hues for `chip_planning`, `chip_discovering` and
/// `accent` — bright chips read well on the near-black background —
/// diverging from the Light twin while keeping the semantic hue families,
/// with `accent` lighter than the DISCOVERING chip (T112.1).
/// `highlighted_text` takes the palette's violet, a canonical Solarized
/// accent no other class uses, so the theme's highlight is its own (T119.1).
const SOLARIZED_DARK_CLASSES: Classes = Classes {
    background: SOLARIZED_DARK.background,
    foreground: SOLARIZED_DARK.foreground,
    normal_text: SOLARIZED_DARK.foreground,
    muted_text: (0x6C, 0x87, 0x8F),
    highlighted_text: (0x6C, 0x71, 0xC4),
    chip_neutral: SOLARIZED_DARK.foreground,
    contrast_text: SOLARIZED_DARK.background,
    success: SOLARIZED_DARK.green,
    chip_planning: (0xDB, 0x5C, 0x99),
    chip_discovering: (0x4C, 0xA2, 0xDF),
    accent: (0x78, 0xB9, 0xE6),
    scrollbar_rail: SOLARIZED_DARK.muted,
};

const SOLARIZED_LIGHT: Palette = Palette {
    background: (0xFD, 0xF6, 0xE3),
    foreground: (0x65, 0x7B, 0x83),
    muted: (0x93, 0xA1, 0xA1),
    surface: (0xEE, 0xE8, 0xD5),
    green: (0x85, 0x99, 0x00),
    blue: (0x26, 0x8B, 0xD2),
    magenta: (0xD3, 0x36, 0x82),
};

/// Light theme: `chip_neutral` takes the foreground role so the
/// background-role contrast text reads on it, `muted_text` takes a darkened
/// muted literal, and the green — short both as a chip background
/// under the contrast text and as text on the background — takes a darkened
/// literal of its hue as `success`; `highlighted_text` takes the palette's
/// violet darkened — the twins share roles, so the light twin darkens — so
/// the theme's highlight is its own (T119.1). `accent`
/// takes a darkened blue literal, deeper than the DISCOVERING chip's raw
/// blue, while the chip backgrounds keep the raw roles the Dark twin
/// brightens (T112.1).
const SOLARIZED_LIGHT_CLASSES: Classes = Classes {
    background: SOLARIZED_LIGHT.background,
    foreground: SOLARIZED_LIGHT.foreground,
    normal_text: SOLARIZED_LIGHT.foreground,
    muted_text: (0x76, 0x87, 0x87),
    highlighted_text: (0x58, 0x5C, 0xC2),
    chip_neutral: SOLARIZED_LIGHT.foreground,
    contrast_text: SOLARIZED_LIGHT.background,
    success: (0x7C, 0x8F, 0x00),
    chip_planning: SOLARIZED_LIGHT.magenta,
    chip_discovering: SOLARIZED_LIGHT.blue,
    accent: (0x21, 0x7A, 0xB8),
    scrollbar_rail: SOLARIZED_LIGHT.muted,
};

const GRUVBOX_DARK: Palette = Palette {
    background: (0x28, 0x28, 0x28),
    foreground: (0xEB, 0xDB, 0xB2),
    muted: (0x92, 0x83, 0x74),
    surface: (0x3C, 0x38, 0x36),
    green: (0xB8, 0xBB, 0x26),
    blue: (0x83, 0xA5, 0x98),
    magenta: (0xD3, 0x86, 0x9B),
};

/// Gruvbox Dark's own muted and accents already clear [`READABLE_CONTRAST`]
/// on its background, so `chip_neutral` keeps the foreground role; `accent`
/// takes a lightened teal literal, brighter than the DISCOVERING chip's raw
/// blue, so the theme's classes are its own (T112.1). `highlighted_text`
/// takes the palette's signature bright orange, so the theme's highlight is
/// its own (T119.1).
const GRUVBOX_DARK_CLASSES: Classes = Classes {
    background: GRUVBOX_DARK.background,
    foreground: GRUVBOX_DARK.foreground,
    normal_text: GRUVBOX_DARK.foreground,
    muted_text: GRUVBOX_DARK.muted,
    highlighted_text: (0xFE, 0x80, 0x19),
    chip_neutral: GRUVBOX_DARK.foreground,
    contrast_text: GRUVBOX_DARK.background,
    success: GRUVBOX_DARK.green,
    chip_planning: GRUVBOX_DARK.magenta,
    chip_discovering: GRUVBOX_DARK.blue,
    accent: (0xA1, 0xBA, 0xB1),
    scrollbar_rail: GRUVBOX_DARK.muted,
};

const GRUVBOX_LIGHT: Palette = Palette {
    background: (0xFB, 0xF1, 0xC7),
    foreground: (0x3C, 0x38, 0x36),
    muted: (0x7C, 0x6F, 0x64),
    surface: (0xEB, 0xDB, 0xB2),
    green: (0x79, 0x74, 0x0E),
    blue: (0x07, 0x66, 0x78),
    magenta: (0x8F, 0x3F, 0x71),
};

/// Gruvbox Light's own muted and accents already clear [`READABLE_CONTRAST`]
/// on its background, so `chip_neutral` keeps the foreground role; `accent`
/// takes a darkened teal literal, deeper than the DISCOVERING chip's raw
/// blue, so the theme's classes are its own (T112.1). `highlighted_text`
/// takes the palette's neutral orange, the light variant's counterpart of
/// the Dark twin's bright orange, so the theme's highlight is its own
/// (T119.1).
const GRUVBOX_LIGHT_CLASSES: Classes = Classes {
    background: GRUVBOX_LIGHT.background,
    foreground: GRUVBOX_LIGHT.foreground,
    normal_text: GRUVBOX_LIGHT.foreground,
    muted_text: GRUVBOX_LIGHT.muted,
    highlighted_text: (0xD6, 0x5D, 0x0A),
    chip_neutral: GRUVBOX_LIGHT.foreground,
    contrast_text: GRUVBOX_LIGHT.background,
    success: GRUVBOX_LIGHT.green,
    chip_planning: GRUVBOX_LIGHT.magenta,
    chip_discovering: GRUVBOX_LIGHT.blue,
    accent: (0x06, 0x52, 0x60),
    scrollbar_rail: GRUVBOX_LIGHT.muted,
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

/// The class table of a non-default theme key (T111.1).
fn classes_of(theme: ThemeKey) -> &'static Classes {
    match theme {
        ThemeKey::AtomOneDark => &ATOM_ONE_DARK_CLASSES,
        ThemeKey::AtomOneLight => &ATOM_ONE_LIGHT_CLASSES,
        ThemeKey::TokyoNightDark => &TOKYO_NIGHT_DARK_CLASSES,
        ThemeKey::TokyoNightDay => &TOKYO_NIGHT_DAY_CLASSES,
        ThemeKey::CatppuccinMocha => &CATPPUCCIN_MOCHA_CLASSES,
        ThemeKey::CatppuccinLatte => &CATPPUCCIN_LATTE_CLASSES,
        ThemeKey::SolarizedDark => &SOLARIZED_DARK_CLASSES,
        ThemeKey::SolarizedLight => &SOLARIZED_LIGHT_CLASSES,
        ThemeKey::GruvboxDark => &GRUVBOX_DARK_CLASSES,
        ThemeKey::GruvboxLight => &GRUVBOX_LIGHT_CLASSES,
        ThemeKey::Dark => unreachable!("the default theme is Theme::DARK, not a palette"),
    }
}

impl Palette {
    /// Emits the theme: the palette's own [`Classes`] table (T111.1) supplies
    /// every class, and the palette's background fixes the agent line colours
    /// (T108.1). The truecolor setting picks the emission, exactly like the
    /// agent text roles.
    fn theme(&self, classes: &Classes, truecolor: bool) -> Theme {
        let colour = |role: (u8, u8, u8)| {
            if truecolor {
                Color::Rgb(role.0, role.1, role.2)
            } else {
                Color::Indexed(rgb_to_indexed(role))
            }
        };
        Theme {
            background: colour(classes.background),
            foreground: colour(classes.foreground),
            normal_text: colour(classes.normal_text),
            muted_text: colour(classes.muted_text),
            highlighted_text: colour(classes.highlighted_text),
            agent_text: AgentText::resolve(self.background, truecolor),
            agent_names: AgentNames::resolve(self.background, truecolor),
            chip_neutral: colour(classes.chip_neutral),
            contrast_text: colour(classes.contrast_text),
            success: colour(classes.success),
            chip_planning: colour(classes.chip_planning),
            chip_discovering: colour(classes.chip_discovering),
            accent: colour(classes.accent),
            scrollbar_rail: colour(classes.scrollbar_rail),
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

/// The theme picker modal's two groups (T114.1, T116.1): [`THEME_KEYS`]
/// classified by [`ThemeKey::is_dark`] into a "Dark" group followed by a
/// "Light" group, each a collapsible section of the picker. Derived from the
/// classification, never from hardcoded indexes; the stable partition keeps
/// each group's `THEME_KEYS` relative order. `THEME_KEYS` itself stays the
/// settings overlay's cycle order.
pub fn theme_modal_groups() -> [(&'static str, Vec<&'static str>); 2] {
    let (dark, light): (Vec<_>, Vec<_>) = patok_core::config::THEME_KEYS
        .iter()
        .copied()
        .partition(|name| ThemeKey::parse(name).is_some_and(ThemeKey::is_dark));
    [("Dark", dark), ("Light", light)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use patok_core::config::THEME_KEYS;

    /// DARK is value-locked to the pre-class-system look (T111.1): every
    /// class keeps the colour the collapsed fields held before the refactor,
    /// and the base style is the terminal default (Reset/Reset), so painting
    /// it is a no-op.
    #[test]
    fn dark_theme_is_the_current_look() {
        let dark = Theme::DARK;
        assert_eq!(dark.background, Color::Reset);
        assert_eq!(dark.foreground, Color::Reset);
        assert_eq!(dark.normal_text, Color::Reset);
        assert_eq!(dark.muted_text, Color::DarkGray);
        assert_eq!(dark.highlighted_text, Color::Yellow);
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
        assert_eq!(dark.chip_neutral, Color::DarkGray);
        assert_eq!(dark.contrast_text, Color::Black);
        assert_eq!(dark.success, Color::Green);
        assert_eq!(dark.chip_planning, Color::Magenta);
        assert_eq!(dark.chip_discovering, Color::Blue);
        assert_eq!(dark.accent, Color::Cyan);
        assert_eq!(dark.scrollbar_rail, Color::DarkGray);
    }

    /// Every field of a theme, in a fixed order, for completeness checks.
    fn every_field(theme: &Theme) -> Vec<Color> {
        let mut fields = every_class_field(theme);
        fields.extend([
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
        ]);
        fields
    }

    /// Every class of a theme, in a fixed order matching the [`Classes`]
    /// struct, for the per-theme table checks. The agent subclasses are
    /// asserted separately (T108.1, T110.1).
    fn every_class_field(theme: &Theme) -> Vec<Color> {
        vec![
            theme.background,
            theme.foreground,
            theme.normal_text,
            theme.muted_text,
            theme.highlighted_text,
            theme.chip_neutral,
            theme.contrast_text,
            theme.success,
            theme.chip_planning,
            theme.chip_discovering,
            theme.accent,
            theme.scrollbar_rail,
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

    /// Every class resolves from the theme's own [`Classes`] table (T111.1):
    /// a theme that silently fell back to a shared mapping rule would fail
    /// this. A class added to `Theme` without a `Classes` counterpart fails
    /// to compile. The agent text and agent name fields are asserted
    /// separately above (T108.1, T110.1). Checked in truecolor mode; the
    /// 256-colour mode derives from the same triples.
    #[test]
    fn class_mapping_is_per_theme() {
        for key in every_key().into_iter().filter(|key| *key != ThemeKey::Dark) {
            let theme = Theme::resolve(key, Some(true));
            let classes = classes_of(key);
            let expected = [
                classes.background,
                classes.foreground,
                classes.normal_text,
                classes.muted_text,
                classes.highlighted_text,
                classes.chip_neutral,
                classes.contrast_text,
                classes.success,
                classes.chip_planning,
                classes.chip_discovering,
                classes.accent,
                classes.scrollbar_rail,
            ];
            for (got, want) in every_class_field(&theme).into_iter().zip(expected) {
                assert_eq!(
                    got,
                    Color::Rgb(want.0, want.1, want.2),
                    "{key:?} class must resolve from its per-theme table"
                );
            }
        }
    }

    /// Every class value is genuinely its theme's own (T112.1): for each of
    /// the twelve class fields no two of the eleven built-in themes resolve
    /// to the same colour, so switching themes changes the whole app look
    /// instead of re-hueing one shared mapping. Dark's named ANSI colours
    /// can never equal a palette's `Color::Rgb`, so it takes part in the
    /// comparison. `accent` additionally stays distinct from
    /// `chip_discovering` in every theme, so the active rail tile and
    /// cursor never wear the DISCOVERING chip's colour.
    #[test]
    fn class_values_genuinely_differ_across_themes() {
        const CLASS_NAMES: [&str; 12] = [
            "background",
            "foreground",
            "normal_text",
            "muted_text",
            "highlighted_text",
            "chip_neutral",
            "contrast_text",
            "success",
            "chip_planning",
            "chip_discovering",
            "accent",
            "scrollbar_rail",
        ];
        let themes: Vec<Theme> = every_key()
            .into_iter()
            .map(|key| Theme::resolve(key, Some(true)))
            .collect();
        let fields: Vec<Vec<Color>> = themes.iter().map(every_class_field).collect();
        for (index, class) in CLASS_NAMES.iter().enumerate() {
            for (i, left) in fields.iter().enumerate() {
                for right in &fields[i + 1..] {
                    assert_ne!(
                        left[index], right[index],
                        "two built-in themes share a {class} value"
                    );
                }
            }
        }
        for theme in &themes {
            assert_ne!(
                theme.accent, theme.chip_discovering,
                "accent must stay distinct from the DISCOVERING chip"
            );
        }
    }

    /// `highlighted_text` is each built-in theme's own signature accent
    /// (T119.1) instead of the shared yellow convention the tables first
    /// used: the default dark theme keeps its value-locked yellow, every
    /// other theme's value differs from it and is unique among the eleven,
    /// and each non-dark theme resolves the class straight from its own
    /// [`Classes`] table, so every consumer reading the class off the active
    /// theme follows the theme's definition. Atom One Dark's highlight
    /// moved from its signature cyan to the planner-name magenta family
    /// (T120.1), still unique among the eleven and clear of the
    /// readability bar.
    #[test]
    fn highlighted_text_is_theme_specific() {
        // The default's value lock, explicit for this class.
        assert_eq!(Theme::DARK.highlighted_text, Color::Yellow);
        // Atom One Dark's own lock (T120.1): the highlight moved from the
        // signature cyan to the planner-name magenta family.
        assert_eq!(
            classes_of(ThemeKey::AtomOneDark).highlighted_text,
            (0xBF, 0x72, 0xE0)
        );
        for key in every_key().into_iter().filter(|key| *key != ThemeKey::Dark) {
            let theme = Theme::resolve(key, Some(true));
            assert_ne!(
                theme.highlighted_text,
                Theme::DARK.highlighted_text,
                "{key:?} must not reuse the default theme's highlighted text"
            );
            let classes = classes_of(key);
            assert_eq!(
                theme.highlighted_text,
                Color::Rgb(
                    classes.highlighted_text.0,
                    classes.highlighted_text.1,
                    classes.highlighted_text.2
                ),
                "{key:?} must resolve highlighted_text from its own table"
            );
        }
        // Pairwise uniqueness across all eleven themes: the default's named
        // `Color::Yellow` can never equal a palette's `Color::Rgb`.
        let themes: Vec<Theme> = every_key()
            .into_iter()
            .map(|key| Theme::resolve(key, Some(true)))
            .collect();
        for (i, left) in themes.iter().enumerate() {
            for right in &themes[i + 1..] {
                assert_ne!(
                    left.highlighted_text, right.highlighted_text,
                    "two built-in themes share a highlighted_text value"
                );
            }
        }
    }

    /// The shared contrast text reads on every coloured background of every
    /// theme: after the class collapse (T111.1) each chip and cursor
    /// background reaches [`READABLE_CONTRAST`] under `contrast_text` —
    /// including Tokyo Night Dark, whose `chip_neutral` moved off the
    /// palette's dim `surface` role in T112.1. The default theme is excluded
    /// because its background is the terminal default (`Color::Reset`),
    /// whose colour the theme cannot know.
    #[test]
    fn contrast_text_reads_on_every_colored_background() {
        for key in every_key().into_iter().filter(|key| *key != ThemeKey::Dark) {
            let classes = classes_of(key);
            for background in [
                classes.chip_neutral,
                classes.success,
                classes.chip_planning,
                classes.chip_discovering,
                classes.highlighted_text,
                classes.accent,
            ] {
                let ratio = contrast_ratio(classes.contrast_text, background);
                assert!(
                    ratio >= READABLE_CONTRAST,
                    "{key:?} contrast text reaches only {ratio} on a coloured background"
                );
            }
        }
    }

    /// Every palette theme's class text reads on its own background
    /// (T112.1): foreground, normal, muted, highlighted text, `success`
    /// and `accent` all reach [`READABLE_CONTRAST`], and the decorative
    /// neutral-chip and scrollbar-rail backgrounds merely differ from it.
    /// The dim-muted aesthetic the light-only version of this test allowed
    /// was retired in T112.1: dark themes now carry brightened muted
    /// literals. The default theme stays excluded because its background is
    /// the terminal default (`Color::Reset`), whose colour the theme cannot
    /// know; its identity is value-locked by the DARK test above.
    #[test]
    fn every_palette_theme_class_text_is_readable() {
        for key in every_key().into_iter().filter(|key| *key != ThemeKey::Dark) {
            let classes = classes_of(key);
            let text_classes = [
                ("foreground", classes.foreground),
                ("normal_text", classes.normal_text),
                ("muted_text", classes.muted_text),
                ("highlighted_text", classes.highlighted_text),
                ("success", classes.success),
                ("accent", classes.accent),
            ];
            for (class, text) in text_classes {
                let ratio = contrast_ratio(text, classes.background);
                assert!(
                    ratio >= READABLE_CONTRAST,
                    "{key:?} {class} reaches only {ratio} on the background"
                );
            }
            assert_ne!(classes.chip_neutral, classes.background);
            assert_ne!(classes.scrollbar_rail, classes.background);
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

    /// The picker's groups (T114.1, T116.1) cover every built-in exactly once:
    /// the first group is titled Dark and holds every dark theme, the second
    /// is titled Light and holds every light theme.
    #[test]
    fn the_modal_groups_cover_every_theme_exactly_once() {
        let groups = theme_modal_groups();
        assert_eq!(groups[0].0, "Dark");
        assert_eq!(groups[1].0, "Light");
        assert!(!groups[0].1.is_empty());
        assert!(!groups[1].1.is_empty());
        // The same multiset across both groups: every built-in appears exactly once.
        let mut all: Vec<&str> = groups[0]
            .1
            .iter()
            .chain(groups[1].1.iter())
            .copied()
            .collect();
        assert_eq!(all.len(), THEME_KEYS.len());
        all.sort_unstable();
        let mut expected = THEME_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(all, expected);
        // The classification lands every entry in the group it belongs to.
        let is_dark = |name: &str| ThemeKey::parse(name).is_some_and(ThemeKey::is_dark);
        assert!(
            groups[0].1.iter().all(|name| is_dark(name)),
            "every entry under Dark is dark"
        );
        assert!(
            groups[1].1.iter().all(|name| !is_dark(name)),
            "every entry under Light is light"
        );
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
