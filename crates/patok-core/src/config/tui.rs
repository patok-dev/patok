//! The tui schema: display and interaction preferences, owned
//! by the shell. The engine never reads this schema; it parses only the `[tui]` table of
//! each config file. There are no environment overrides.

use std::fmt;
use std::path::Path;

use schemars::JsonSchema;

use super::{ConfigFiles, Fields, migrate_project_file, project_file, read_layer_table};

/// The shell theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
#[schemars(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Dark,
    AtomOneDark,
    AtomOneLight,
    TokyoNightDark,
    TokyoNightDay,
    CatppuccinMocha,
    CatppuccinLatte,
    SolarizedDark,
    SolarizedLight,
    GruvboxDark,
    GruvboxLight,
}

/// Every theme name the `tui.theme` key accepts, in the settings overlay's cycle
/// order: the default first, then dark/light pairs per family. The config parser,
/// the persistence validator and the overlay row all derive from this list, so
/// they cannot drift apart.
pub const THEME_KEYS: &[&str] = &[
    "dark",
    "atom_one_dark",
    "atom_one_light",
    "tokyo_night_dark",
    "tokyo_night_day",
    "catppuccin_mocha",
    "catppuccin_latte",
    "solarized_dark",
    "solarized_light",
    "gruvbox_dark",
    "gruvbox_light",
];

impl Theme {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "atom_one_dark" => Some(Self::AtomOneDark),
            "atom_one_light" => Some(Self::AtomOneLight),
            "tokyo_night_dark" => Some(Self::TokyoNightDark),
            "tokyo_night_day" => Some(Self::TokyoNightDay),
            "catppuccin_mocha" => Some(Self::CatppuccinMocha),
            "catppuccin_latte" => Some(Self::CatppuccinLatte),
            "solarized_dark" => Some(Self::SolarizedDark),
            "solarized_light" => Some(Self::SolarizedLight),
            "gruvbox_dark" => Some(Self::GruvboxDark),
            "gruvbox_light" => Some(Self::GruvboxLight),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::AtomOneDark => "atom_one_dark",
            Self::AtomOneLight => "atom_one_light",
            Self::TokyoNightDark => "tokyo_night_dark",
            Self::TokyoNightDay => "tokyo_night_day",
            Self::CatppuccinMocha => "catppuccin_mocha",
            Self::CatppuccinLatte => "catppuccin_latte",
            Self::SolarizedDark => "solarized_dark",
            Self::SolarizedLight => "solarized_light",
            Self::GruvboxDark => "gruvbox_dark",
            Self::GruvboxLight => "gruvbox_light",
        }
    }
}

impl fmt::Display for Theme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which releases self-update considers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
#[schemars(rename_all = "lowercase")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Dev,
}

impl UpdateChannel {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "stable" => Some(Self::Stable),
            "dev" => Some(Self::Dev),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Dev => "dev",
        }
    }
}

impl fmt::Display for UpdateChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The pipeline rail's visual mode (T57.1): `normal` full-name boxes (the default since T58.1) or `compact` letter tiles, the opt-in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
#[schemars(rename_all = "lowercase")]
pub enum RailMode {
    Compact,
    #[default]
    Normal,
    Detailed,
}

impl RailMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "compact" => Some(Self::Compact),
            "normal" => Some(Self::Normal),
            "detailed" => Some(Self::Detailed),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Normal => "normal",
            Self::Detailed => "detailed",
        }
    }
}

impl fmt::Display for RailMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The agent pane split's valid range.
pub const AGENT_PANE_SPLIT_MIN: u64 = 20;
pub const AGENT_PANE_SPLIT_MAX: u64 = 80;

/// The merged tui settings.
#[derive(Clone, Debug, PartialEq, JsonSchema)]
pub struct TuiSettings {
    pub theme: Theme,
    /// `None` means auto-detect; `Some(true)` forces RGB, `Some(false)` forces 256-color.
    pub truecolor: Option<bool>,
    pub preview_wrap: bool,
    /// The agent pane split, valid range 20-80.
    pub agent_pane_split: u64,
    pub update_channel: UpdateChannel,
    pub rail_mode: RailMode,
}

impl Default for TuiSettings {
    fn default() -> Self {
        TuiLayer::default().materialize()
    }
}

/// One layer's `[tui]` table; unset keys fall through to the layer below.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TuiLayer {
    pub theme: Option<Theme>,
    pub truecolor: Option<bool>,
    pub preview_wrap: Option<bool>,
    pub agent_pane_split: Option<u64>,
    pub update_channel: Option<UpdateChannel>,
    pub rail_mode: Option<RailMode>,
}

impl TuiLayer {
    pub(crate) fn materialize(self) -> TuiSettings {
        TuiSettings {
            theme: self.theme.unwrap_or_default(),
            truecolor: self.truecolor,
            preview_wrap: self.preview_wrap.unwrap_or(true),
            agent_pane_split: self.agent_pane_split.unwrap_or(50),
            update_channel: self.update_channel.unwrap_or_default(),
            rail_mode: self.rail_mode.unwrap_or_default(),
        }
    }
}

/// Loads and merges the tui schema across the three file layers. The project layer
/// lives in `data_dir` (the project's slot of the projects dir), and a legacy
/// project-root config is migrated there first; `None` skips the project layer.
pub fn load_tui(
    files: &ConfigFiles,
    project_dir: &Path,
    data_dir: Option<&Path>,
) -> (TuiSettings, Vec<String>) {
    let mut warnings = Vec::new();
    let mut merged = TuiLayer::default();
    let project_path = data_dir.map(|dir| {
        migrate_project_file(project_dir, dir, &mut warnings);
        project_file(dir)
    });
    for path in [
        files.user_global.as_deref(),
        files.user_local.as_deref(),
        project_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let Some(table) = read_layer_table(path, "tui", "daemon", &mut warnings) else {
            continue;
        };
        let layer = parse_layer(&table, path, &mut warnings);
        merged = super::merge::tui(merged, layer);
    }
    (merged.materialize(), warnings)
}

/// The help text for one settings-overlay key of the tui schema (T85.1):
/// the description of a setting row, or of the whole display section when the
/// key is its group id (`display_and_theme`). Hardcoded here, on the side that owns the settings it
/// describes; the shell routes to it by the row's declared schema. `None` for
/// a key that is not one of the overlay's tui rows or groups.
pub fn tui_help(key: &str) -> Option<&'static str> {
    Some(match key {
        "display_and_theme" => {
            "The shell's own display preferences. The shell applies and persists them itself -- no engine involved -- and each takes effect immediately."
        }
        "theme" => {
            "Colour theme: dark is the default; the other keys are published palettes in dark and light pairs. Applies immediately."
        }
        "truecolor" => {
            "auto detects the terminal's colour depth; on forces RGB, off forces the nearest 256-colour. Default auto."
        }
        "preview_wrap" => "Wrap long lines in the output preview (default on).",
        "update_channel" => "Which GitHub releases self-update considers: stable (default) or dev.",
        "rail_mode" => "The pipeline rail's density: compact, normal or detailed.",
        _ => return None,
    })
}

/// Parses one file's `[tui]` table into a raw layer.
pub(crate) fn parse_layer(
    table: &toml::Table,
    path: &Path,
    warnings: &mut Vec<String>,
) -> TuiLayer {
    let mut layer = TuiLayer::default();
    let mut f = Fields {
        path,
        schema: "tui",
        warnings,
    };
    for (key, value) in table {
        match key.as_str() {
            "theme" => {
                layer.theme = f.enumerated(
                    key,
                    value,
                    &format!("`{}`", THEME_KEYS.join("`, `")),
                    Theme::parse,
                );
            }
            "truecolor" => layer.truecolor = f.boolean(key, value),
            "preview_wrap" => layer.preview_wrap = f.boolean(key, value),
            "agent_pane_split" => layer.agent_pane_split = f.split(key, value),
            "update_channel" => {
                layer.update_channel =
                    f.enumerated(key, value, "`stable`, `dev`", UpdateChannel::parse);
            }
            "rail_mode" => {
                layer.rail_mode = f.enumerated(
                    key,
                    value,
                    "`compact`, `normal`, `detailed`",
                    RailMode::parse,
                );
            }
            other => f.unknown(other),
        }
    }
    layer
}

impl Fields<'_> {
    /// The agent pane split: a whole number clamped into the 20-80 range with a warning
    /// naming the field and the source file.
    fn split(&mut self, key: &str, value: &toml::Value) -> Option<u64> {
        let Some(split) = value.as_integer().and_then(|i| u64::try_from(i).ok()) else {
            self.warnings.push(format!(
                "{}: tui.{key} must be a whole number between {AGENT_PANE_SPLIT_MIN} and {AGENT_PANE_SPLIT_MAX}, got {value}; using the default",
                self.path.display()
            ));
            return None;
        };
        if (AGENT_PANE_SPLIT_MIN..=AGENT_PANE_SPLIT_MAX).contains(&split) {
            return Some(split);
        }
        let clamped = split.clamp(AGENT_PANE_SPLIT_MIN, AGENT_PANE_SPLIT_MAX);
        self.warnings.push(format!(
            "{}: tui.{key} must be between {AGENT_PANE_SPLIT_MIN} and {AGENT_PANE_SPLIT_MAX}, got {split}; clamping to {clamped}",
            self.path.display()
        ));
        Some(clamped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `normal` is the rail's default mode (T58.1): the enum default every
    /// unset or invalid `rail_mode` falls back to.
    #[test]
    fn normal_is_the_default_rail_mode() {
        assert_eq!(RailMode::default(), RailMode::Normal);
        assert_eq!(TuiSettings::default().rail_mode, RailMode::Normal);
    }

    /// A fresh or empty config resolves `rail_mode` to `normal` with no
    /// warnings: only an explicit `compact` selects the letter tiles.
    #[test]
    fn a_fresh_config_resolves_the_rail_mode_to_normal() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, warnings) = load_tui(&ConfigFiles::default(), dir.path(), None);
        assert_eq!(settings.rail_mode, RailMode::Normal);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    /// The help registry (T85.1) answers every overlay key it owns with
    /// non-empty text, and nothing else.
    #[test]
    fn tui_help_covers_its_keys_and_rejects_unknown_ones() {
        for key in ["theme", "rail_mode", "display_and_theme"] {
            let help = tui_help(key).unwrap_or_else(|| panic!("`{key}` has no help text"));
            assert!(!help.is_empty());
        }
        // A field the overlay deliberately excludes stays out, as do keys of
        // the other schema.
        assert_eq!(tui_help("agent_pane_split"), None);
        assert_eq!(tui_help("run_mode"), None);
    }
}
