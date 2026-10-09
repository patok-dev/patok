//! The tui schema: display and interaction preferences, owned
//! by the shell. The engine never reads this schema; it parses only the `[tui]` table of
//! each config file. There are no environment overrides.

use std::path::Path;

use schemars::JsonSchema;

use super::spelling::config_enum;
use super::{
    ConfigFiles, EnumSpelling, Fields, migrate_project_file, project_file, read_layer_table,
};

use crate::update::UpdateChannel;

config_enum! {
    /// The shell theme.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
    #[schemars(rename_all = "snake_case")]
    pub enum Theme {
        #[default]
        Dark => "dark",
        AtomOneDark => "atom_one_dark",
        AtomOneLight => "atom_one_light",
        TokyoNightDark => "tokyo_night_dark",
        TokyoNightDay => "tokyo_night_day",
        CatppuccinMocha => "catppuccin_mocha",
        CatppuccinLatte => "catppuccin_latte",
        SolarizedDark => "solarized_dark",
        SolarizedLight => "solarized_light",
        GruvboxDark => "gruvbox_dark",
        GruvboxLight => "gruvbox_light",
    }
}

/// Every theme name the `tui.theme` key accepts, in the settings overlay's cycle
/// order: the default first, then dark/light pairs per family. Emitted from the
/// same spelling list as [`EnumSpelling::VARIANTS`], so the config parser, the
/// persistence validator and the overlay row cannot drift apart.
pub const THEME_KEYS: &[&str] = Theme::SPELLINGS;

impl Theme {
    /// Whether the theme is a dark variant: the default `dark` built-in and the
    /// five dark palettes. The theme picker's modal groups on this (T114.1).
    pub fn is_dark(self) -> bool {
        matches!(
            self,
            Self::Dark
                | Self::AtomOneDark
                | Self::TokyoNightDark
                | Self::CatppuccinMocha
                | Self::SolarizedDark
                | Self::GruvboxDark
        )
    }
}

config_enum! {
    /// The pipeline rail's visual mode (T57.1): `normal` full-name boxes (the default since T58.1) or `compact` letter tiles, the opt-in.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
    #[schemars(rename_all = "lowercase")]
    pub enum RailMode {
        Compact => "compact",
        #[default]
        Normal => "normal",
        Detailed => "detailed",
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
        "update_channel" => {
            "Which GitHub releases self-update considers: stable (default) or dev. patok update uses this unless --channel is given."
        }
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
                layer.theme = f.enumerated(key, value, &Theme::accepted_spellings(), Theme::parse);
            }
            "truecolor" => layer.truecolor = f.boolean(key, value),
            "preview_wrap" => layer.preview_wrap = f.boolean(key, value),
            "agent_pane_split" => layer.agent_pane_split = f.split(key, value),
            "update_channel" => {
                layer.update_channel = f.enumerated(
                    key,
                    value,
                    &UpdateChannel::accepted_spellings(),
                    UpdateChannel::parse,
                );
            }
            "rail_mode" => {
                layer.rail_mode =
                    f.enumerated(key, value, &RailMode::accepted_spellings(), RailMode::parse);
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
