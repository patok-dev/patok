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
//! | `thinking` | `style_of` | agent thinking; also the markdown base style |
//! | `tool` | `style_of` | tool-call lines |
//! | `result` | `style_of` | result lines |
//! | `error` | `style_of` | error lines |
//! | `notice` | `style_of` | notice lines |
//! | `heading` | `style_of` | markdown headings (bold added by the renderer) |
//! | `frame_title_builder` | `render_output` | the agent type name in the output frame title while the builder runs |
//! | `frame_title_planner` | `render_output` | the agent type name in the output frame title while the planner runs |
//! | `frame_title_research` | `render_output` | the agent type name in the output frame title while the research agent runs (T68.1) |
//! | `frame_title_detail` | `render_output`, `render_tasks` | output frame title's separator, provider, model and timer; everything after the word `Tasks` in the tasks frame title (T67.1): the pipe separator, the completed, total and left counts, the slash, the dash and the word `left` |
//! | `pane_status` | `style_of` | the agent lifecycle status lines in the output pane: the started line (T78.1) and the finished line (T42.1), in the same colour as the heading/task lines (T96.1) |
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
    /// Agent thinking lines; also the base style markdown rendering inherits.
    pub thinking: Color,
    /// Tool-call lines.
    pub tool: Color,
    /// Result lines.
    pub result: Color,
    /// Error lines.
    pub error: Color,
    /// Notice lines.
    pub notice: Color,
    /// Markdown headings (the renderer adds bold).
    pub heading: Color,
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
    /// The agent lifecycle status lines in the output pane: the started line
    /// (T78.1) and the finished line (T42.1), in the same colour as the
    /// heading/task lines (T96.1).
    pub pane_status: Color,
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
        thinking: Color::Blue,
        tool: Color::Cyan,
        result: Color::DarkGray,
        error: Color::Red,
        notice: Color::Yellow,
        heading: Color::Magenta,
        frame_title_builder: Color::Green,
        frame_title_planner: Color::Magenta,
        frame_title_research: Color::LightBlue,
        frame_title_detail: Color::DarkGray,
        pane_status: Color::Magenta,
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
    /// mapping, shared by the frame's `style_of` and the headless mode's
    /// `--color` output so the two cannot drift. `Text` lines render unstyled
    /// (the caller decides what that means), so it maps to `Color::Reset`.
    pub fn line_color(theme: Theme, kind: LineKind) -> Color {
        match kind {
            LineKind::Text => Color::Reset,
            LineKind::Thinking => theme.thinking,
            LineKind::Tool => theme.tool,
            LineKind::Result => theme.result,
            LineKind::Error => theme.error,
            LineKind::Notice => theme.notice,
            LineKind::Status => theme.pane_status,
            LineKind::Heading => theme.heading,
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
    red: (u8, u8, u8),
    green: (u8, u8, u8),
    /// The palette's warning colour.
    yellow: (u8, u8, u8),
    /// The palette's accent.
    blue: (u8, u8, u8),
    cyan: (u8, u8, u8),
    magenta: (u8, u8, u8),
}

const ATOM_ONE_DARK: Palette = Palette {
    background: (0x28, 0x2C, 0x34),
    foreground: (0xAB, 0xB2, 0xBF),
    muted: (0x5C, 0x63, 0x70),
    surface: (0x3E, 0x44, 0x51),
    red: (0xE0, 0x6C, 0x75),
    green: (0x98, 0xC3, 0x79),
    yellow: (0xE5, 0xC0, 0x7B),
    blue: (0x61, 0xAF, 0xEF),
    cyan: (0x56, 0xB6, 0xC2),
    magenta: (0xC6, 0x78, 0xDD),
};

const ATOM_ONE_LIGHT: Palette = Palette {
    background: (0xFA, 0xFA, 0xFA),
    foreground: (0x38, 0x3A, 0x42),
    muted: (0xA0, 0xA1, 0xA7),
    surface: (0xE5, 0xE5, 0xE5),
    red: (0xE4, 0x56, 0x49),
    green: (0x50, 0xA1, 0x4F),
    yellow: (0xC1, 0x84, 0x01),
    blue: (0x40, 0x78, 0xF2),
    cyan: (0x01, 0x84, 0xBC),
    magenta: (0xA6, 0x26, 0xA4),
};

const TOKYO_NIGHT_DARK: Palette = Palette {
    background: (0x1A, 0x1B, 0x26),
    foreground: (0xC0, 0xCA, 0xF5),
    muted: (0x56, 0x5F, 0x89),
    surface: (0x33, 0x46, 0x7C),
    red: (0xF7, 0x76, 0x8E),
    green: (0x9E, 0xCE, 0x6A),
    yellow: (0xE0, 0xAF, 0x68),
    blue: (0x7A, 0xA2, 0xF7),
    cyan: (0x7D, 0xCF, 0xFF),
    magenta: (0xBB, 0x9A, 0xF7),
};

const TOKYO_NIGHT_DAY: Palette = Palette {
    background: (0xE1, 0xE2, 0xE7),
    foreground: (0x37, 0x60, 0xBF),
    muted: (0x84, 0x8C, 0xB5),
    surface: (0xC4, 0xC8, 0xDA),
    red: (0xF5, 0x2A, 0x65),
    green: (0x58, 0x75, 0x39),
    yellow: (0x8C, 0x6C, 0x3E),
    blue: (0x2E, 0x7D, 0xE9),
    cyan: (0x38, 0xA1, 0xD6),
    magenta: (0x98, 0x54, 0xF1),
};

const CATPPUCCIN_MOCHA: Palette = Palette {
    background: (0x1E, 0x1E, 0x2E),
    foreground: (0xCD, 0xD6, 0xF4),
    muted: (0x7F, 0x84, 0x9C),
    surface: (0x31, 0x32, 0x44),
    red: (0xF3, 0x8B, 0xA8),
    green: (0xA6, 0xE3, 0xA1),
    yellow: (0xF9, 0xE2, 0xAF),
    blue: (0x89, 0xB4, 0xFA),
    cyan: (0x94, 0xE2, 0xD5),
    magenta: (0xCB, 0xA6, 0xF7),
};

const CATPPUCCIN_LATTE: Palette = Palette {
    background: (0xEF, 0xF1, 0xF5),
    foreground: (0x4C, 0x4F, 0x69),
    muted: (0x7C, 0x7F, 0x94),
    surface: (0xCC, 0xD0, 0xDA),
    red: (0xD2, 0x0F, 0x39),
    green: (0x40, 0xA0, 0x2B),
    yellow: (0xDF, 0x8E, 0x1D),
    blue: (0x1E, 0x66, 0xF5),
    cyan: (0x17, 0x92, 0x99),
    magenta: (0x88, 0x39, 0xEF),
};

const SOLARIZED_DARK: Palette = Palette {
    background: (0x00, 0x2B, 0x36),
    foreground: (0x83, 0x94, 0x96),
    muted: (0x58, 0x6E, 0x75),
    surface: (0x07, 0x36, 0x42),
    red: (0xDC, 0x32, 0x2F),
    green: (0x85, 0x99, 0x00),
    yellow: (0xB5, 0x89, 0x00),
    blue: (0x26, 0x8B, 0xD2),
    cyan: (0x2A, 0xA1, 0x98),
    magenta: (0xD3, 0x36, 0x82),
};

const SOLARIZED_LIGHT: Palette = Palette {
    background: (0xFD, 0xF6, 0xE3),
    foreground: (0x65, 0x7B, 0x83),
    muted: (0x93, 0xA1, 0xA1),
    surface: (0xEE, 0xE8, 0xD5),
    red: (0xDC, 0x32, 0x2F),
    green: (0x85, 0x99, 0x00),
    yellow: (0xB5, 0x89, 0x00),
    blue: (0x26, 0x8B, 0xD2),
    cyan: (0x2A, 0xA1, 0x98),
    magenta: (0xD3, 0x36, 0x82),
};

const GRUVBOX_DARK: Palette = Palette {
    background: (0x28, 0x28, 0x28),
    foreground: (0xEB, 0xDB, 0xB2),
    muted: (0x92, 0x83, 0x74),
    surface: (0x3C, 0x38, 0x36),
    red: (0xFB, 0x49, 0x34),
    green: (0xB8, 0xBB, 0x26),
    yellow: (0xFA, 0xBD, 0x2F),
    blue: (0x83, 0xA5, 0x98),
    cyan: (0x8E, 0xC0, 0x7C),
    magenta: (0xD3, 0x86, 0x9B),
};

const GRUVBOX_LIGHT: Palette = Palette {
    background: (0xFB, 0xF1, 0xC7),
    foreground: (0x3C, 0x38, 0x36),
    muted: (0x7C, 0x6F, 0x64),
    surface: (0xEB, 0xDB, 0xB2),
    red: (0x9D, 0x00, 0x06),
    green: (0x79, 0x74, 0x0E),
    yellow: (0xB5, 0x76, 0x14),
    blue: (0x07, 0x66, 0x78),
    cyan: (0x42, 0x7B, 0x58),
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
        let red = colour(self.red);
        let green = colour(self.green);
        let yellow = colour(self.yellow);
        let blue = colour(self.blue);
        let cyan = colour(self.cyan);
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
            thinking: blue,
            tool: cyan,
            result: muted,
            error: red,
            notice: yellow,
            heading: magenta,
            frame_title_builder: green,
            frame_title_planner: magenta,
            frame_title_research: blue,
            frame_title_detail: muted,
            pane_status: magenta,
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
        assert_eq!(dark.thinking, Color::Blue);
        assert_eq!(dark.tool, Color::Cyan);
        assert_eq!(dark.result, Color::DarkGray);
        assert_eq!(dark.error, Color::Red);
        assert_eq!(dark.notice, Color::Yellow);
        assert_eq!(dark.heading, Color::Magenta);
        assert_eq!(dark.frame_title_builder, Color::Green);
        assert_eq!(dark.frame_title_planner, Color::Magenta);
        assert_eq!(dark.frame_title_research, Color::LightBlue);
        assert_eq!(dark.frame_title_detail, Color::DarkGray);
        assert_eq!(dark.pane_status, Color::Magenta);
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
            theme.thinking,
            theme.tool,
            theme.result,
            theme.error,
            theme.notice,
            theme.heading,
            theme.frame_title_builder,
            theme.frame_title_planner,
            theme.frame_title_research,
            theme.frame_title_detail,
            theme.pane_status,
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
