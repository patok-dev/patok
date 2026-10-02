//! The layered configuration: two independent schemas that can live side
//! by side in one TOML file, each parsed only by the process that owns it.
//!
//! Layers, lowest to highest: hardcoded defaults, user-global `config.toml`, user-local
//! `config.local.toml` (both under the XDG config dir with the `~/.config` fallback) and
//! project-local `patok.config.toml` inside the project's slot of the projects dir (a
//! legacy `patok.config.toml` in the project root is migrated there on first load);
//! the daemon schema additionally takes environment
//! overrides. Merge is deep, key by key; an unreadable or unparseable file is skipped with
//! a warning, and a field with a wrong type falls back to its default with a warning
//! naming the field and the source file while every other field is kept.

mod daemon;
mod merge;
mod persist;
mod schema;
mod tui;

pub use daemon::{
    AcceptPolicy, DaemonConfig, DaemonEnv, DaemonSettings, ProviderKind, ProviderName, RunMode,
    Stage, daemon_help, load_daemon,
};
pub use persist::{
    ApplyTiming, PersistError, SettingValue, daemon_apply_timing, daemon_changed_fields,
    daemon_field_value, daemon_readout, set_daemon_field, set_tui_field,
};
pub use schema::{daemon_json_schema, tui_json_schema};
pub use tui::{RailMode, THEME_KEYS, Theme, TuiSettings, UpdateChannel, load_tui, tui_help};

use std::path::{Path, PathBuf};

/// User-global config file name inside the user config directory (`<config dir>/patok/`).
pub const USER_FILE: &str = "config.toml";
/// User-local config file name, same directory as [`USER_FILE`] (per-machine overrides).
pub const USER_LOCAL_FILE: &str = "config.local.toml";
/// Project-local config file name, inside the project's slot of the projects dir.
pub const PROJECT_FILE: &str = "patok.config.toml";

/// Default discovery cooldown: 5 minutes.
pub const DEFAULT_DISCOVERY_COOLDOWN_SECS: u64 = 300;
/// Cap the discovery cooldown doubles up to: 30 minutes.
pub const DEFAULT_DISCOVERY_COOLDOWN_CAP_SECS: u64 = 1800;

/// The two file layers outside the project: the user-global and user-local config files.
/// `None` for a path means that layer is not read (also the case when neither
/// `XDG_CONFIG_HOME` nor `HOME` is set).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigFiles {
    pub user_global: Option<PathBuf>,
    pub user_local: Option<PathBuf>,
}

impl ConfigFiles {
    /// The user layers from the process environment.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        let (xdg, home) = (var("XDG_CONFIG_HOME"), var("HOME"));
        Self {
            user_global: user_global_file(xdg.as_deref(), home.as_deref()),
            user_local: user_local_file(xdg.as_deref(), home.as_deref()),
        }
    }
}

/// The user-global config file under `config_home` (`$XDG_CONFIG_HOME`, else `$HOME/.config`).
pub fn user_global_file(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    user_layer_file(USER_FILE, xdg_config_home, home)
}

/// The user-local config file, in the same directory as the user-global file.
pub fn user_local_file(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    user_layer_file(USER_LOCAL_FILE, xdg_config_home, home)
}

fn user_layer_file(
    file: &str,
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> Option<PathBuf> {
    let base = match xdg_config_home.filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(home.filter(|v| !v.is_empty())?).join(".config"),
    };
    Some(base.join("patok").join(file))
}

/// The project-local config file: `patok.config.toml` in the project's slot of the
/// projects dir (the engine's `data_dir`).
pub fn project_file(data_dir: &Path) -> PathBuf {
    data_dir.join(PROJECT_FILE)
}

/// Migrates a legacy project-root `patok.config.toml` into the project's slot of the
/// projects dir: when the new location holds no config yet but the project root does,
/// the legacy file is copied there byte-for-byte (the legacy file itself is left in
/// place, untouched). Idempotent: a config already at the new location wins and nothing
/// is copied. Any failure is skipped with a warning, like an unreadable config read.
pub fn migrate_project_file(project_dir: &Path, data_dir: &Path, warnings: &mut Vec<String>) {
    let target = project_file(data_dir);
    let legacy = project_dir.join(PROJECT_FILE);
    if target.exists() || !legacy.exists() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(data_dir) {
        warnings.push(format!(
            "cannot create the project config directory {}: {e}",
            data_dir.display()
        ));
        return;
    }
    if let Err(e) = std::fs::copy(&legacy, &target) {
        warnings.push(format!(
            "skipping the migration of {}: {e}",
            legacy.display()
        ));
    }
}

/// Reads one file and returns its `owned` table. A missing file is silent and yields
/// `None`; an unreadable or unparseable file is skipped with a warning (this does not
/// disqualify the other files, or this file's other schema table). The `ignored` table
/// name belongs to the other schema and is skipped silently; any other top-level key
/// warns.
pub(crate) fn read_layer_table(
    path: &Path,
    owned: &str,
    ignored: &str,
    warnings: &mut Vec<String>,
) -> Option<toml::Table> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warnings.push(format!(
                "skipping unreadable config {}: {e}",
                path.display()
            ));
            return None;
        }
    };
    let table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(e) => {
            warnings.push(format!(
                "skipping unparseable config {}: {}",
                path.display(),
                e.to_string().trim_end()
            ));
            return None;
        }
    };
    let mut owned_table = None;
    for (key, value) in &table {
        if key == owned {
            owned_table = value.as_table().cloned();
            if owned_table.is_none() {
                warnings.push(format!(
                    "skipping config table {}: `{owned}` must be a table",
                    path.display()
                ));
            }
        } else if key != ignored {
            warnings.push(format!("{}: ignoring unknown key `{key}`", path.display()));
        }
    }
    owned_table
}

/// Per-field extraction with failure handling: a wrong
/// type falls back to that field's default (the `None` of the raw layer) with a warning
/// naming the field and the source file; every other field is kept.
pub(crate) struct Fields<'a> {
    pub path: &'a Path,
    pub schema: &'a str,
    pub warnings: &'a mut Vec<String>,
}

impl Fields<'_> {
    fn wrong_type(&mut self, key: &str, expect: &str, value: &toml::Value) {
        self.warnings.push(format!(
            "{}: {}.{} must be {}, got {}; using the default",
            self.path.display(),
            self.schema,
            key,
            expect,
            value
        ));
    }

    pub fn unknown(&mut self, key: &str) {
        self.warnings.push(format!(
            "{}: ignoring unknown key {}.{}",
            self.path.display(),
            self.schema,
            key
        ));
    }

    /// A boolean field.
    pub fn boolean(&mut self, key: &str, value: &toml::Value) -> Option<bool> {
        let parsed = value.as_bool();
        if parsed.is_none() {
            self.wrong_type(key, "a boolean", value);
        }
        parsed
    }

    /// A whole-number field; `expect` names the value in warnings, `positive` requires > 0.
    pub fn uint(
        &mut self,
        key: &str,
        value: &toml::Value,
        expect: &str,
        positive: bool,
    ) -> Option<u64> {
        let parsed = value
            .as_integer()
            .and_then(|i| u64::try_from(i).ok())
            .filter(|u| !positive || *u > 0);
        if parsed.is_none() {
            self.wrong_type(key, expect, value);
        }
        parsed
    }

    /// A float field, optionally range-checked.
    pub fn float(
        &mut self,
        key: &str,
        value: &toml::Value,
        range: Option<(f64, f64)>,
    ) -> Option<f64> {
        let parsed = value
            .as_float()
            .filter(|f| range.is_none_or(|(min, max)| (min..=max).contains(f)));
        if parsed.is_none() {
            let expect = match range {
                Some((min, max)) => format!("a number between {min} and {max}"),
                None => "a number".to_string(),
            };
            self.wrong_type(key, &expect, value);
        }
        parsed
    }

    /// A port number (1-65535).
    pub fn port(&mut self, key: &str, value: &toml::Value) -> Option<u16> {
        let parsed = value
            .as_integer()
            .and_then(|i| u16::try_from(i).ok())
            .filter(|p| *p > 0);
        if parsed.is_none() {
            self.wrong_type(key, "a port number (1-65535)", value);
        }
        parsed
    }

    /// A string field; an empty string means "unset" and also yields `None`.
    pub fn string(&mut self, key: &str, value: &toml::Value) -> Option<String> {
        let parsed = value
            .as_str()
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty());
        if parsed.is_none() {
            self.wrong_type(key, "a string", value);
        }
        parsed
    }

    /// A list of strings, replaced wholesale when a higher layer sets it.
    pub fn strings(&mut self, key: &str, value: &toml::Value) -> Option<Vec<String>> {
        let parsed = value.as_array().and_then(|entries| {
            entries
                .iter()
                .map(|e| e.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
        });
        if parsed.is_none() {
            self.wrong_type(key, "an array of strings", value);
        }
        parsed
    }

    /// A string map, merged per key across layers.
    pub fn string_map(
        &mut self,
        key: &str,
        value: &toml::Value,
    ) -> Option<std::collections::BTreeMap<String, String>> {
        let parsed = value.as_table().and_then(|table| {
            table
                .iter()
                .map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect::<Option<std::collections::BTreeMap<_, _>>>()
        });
        if parsed.is_none() {
            self.wrong_type(key, "a table of strings", value);
        }
        parsed
    }

    /// An enumerated field: an unrecognized value falls back to the default with a warning
    /// naming the field and the accepted values.
    pub fn enumerated<T>(
        &mut self,
        key: &str,
        value: &toml::Value,
        accepted: &str,
        parse: impl Fn(&str) -> Option<T>,
    ) -> Option<T> {
        let parsed = value.as_str().and_then(parse);
        if parsed.is_none() {
            self.wrong_type(key, &format!("one of {accepted}"), value);
        }
        parsed
    }

    /// A provider field: the string is trimmed and matched
    /// case-insensitively against the accepted spellings; an unrecognized value is a hard
    /// error naming the file and the offending value (no silent fallback), so it returns
    /// `None` and pushes onto `errors` while the field falls back to its default.
    pub fn provider(
        &mut self,
        key: &str,
        value: &toml::Value,
        errors: &mut Vec<String>,
    ) -> Option<ProviderName> {
        let Some(text) = value.as_str() else {
            self.wrong_type(key, "a provider name", value);
            return None;
        };
        match ProviderName::parse(text) {
            Some(provider) => Some(provider),
            None => {
                errors.push(format!(
                    "invalid config {}: {}.{} must be a provider name ({}), got {value}",
                    self.path.display(),
                    self.schema,
                    key,
                    ProviderName::ACCEPTED_SPELLINGS
                ));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A legacy project-root config migrates into the data dir on the first daemon
    /// load: the values load, the file exists at the new location, and the legacy
    /// file stays byte-identical in the project root.
    #[test]
    fn a_legacy_project_config_migrates_into_the_data_dir_on_load() {
        let project = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let legacy_text = "[daemon]\nplan_enabled = false\n";
        std::fs::write(project.path().join(PROJECT_FILE), legacy_text).unwrap();
        let loaded = load_daemon(
            &ConfigFiles::default(),
            project.path(),
            data.path(),
            &crate::config::DaemonEnv::default(),
        );
        assert!(!loaded.settings.plan_enabled);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(
            std::fs::read_to_string(project_file(data.path())).unwrap(),
            legacy_text
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join(PROJECT_FILE)).unwrap(),
            legacy_text,
            "the legacy file is untouched"
        );
    }

    /// Migration is one-time: once a config exists at the new location it is
    /// authoritative, and a later load does not overwrite it with the legacy file.
    #[test]
    fn a_config_at_the_new_location_wins_over_the_legacy_one() {
        let project = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join(PROJECT_FILE),
            "[daemon]\nplan_enabled = false\n",
        )
        .unwrap();
        std::fs::write(project_file(data.path()), "[daemon]\nplan_enabled = true\n").unwrap();
        let loaded = load_daemon(
            &ConfigFiles::default(),
            project.path(),
            data.path(),
            &crate::config::DaemonEnv::default(),
        );
        assert!(loaded.settings.plan_enabled);
        assert_eq!(
            std::fs::read_to_string(project_file(data.path())).unwrap(),
            "[daemon]\nplan_enabled = true\n",
            "the new location was not overwritten"
        );
    }

    /// The tui schema migrates the same way, and `None` (no data dir resolvable)
    /// skips the project layer without touching anything.
    #[test]
    fn a_legacy_project_config_migrates_for_the_tui_schema_and_none_skips_the_layer() {
        let project = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let legacy_text = "[tui]\npreview_wrap = false\n";
        std::fs::write(project.path().join(PROJECT_FILE), legacy_text).unwrap();
        let (settings, warnings) =
            load_tui(&ConfigFiles::default(), project.path(), Some(data.path()));
        assert!(!settings.preview_wrap);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            std::fs::read_to_string(project_file(data.path())).unwrap(),
            legacy_text
        );

        // `None` (no data dir resolvable) skips the project layer without
        // migrating anything.
        let empty_data = tempfile::tempdir().unwrap();
        let (settings, warnings) = load_tui(&ConfigFiles::default(), project.path(), None);
        assert!(settings.preview_wrap, "the project layer was skipped");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!project_file(empty_data.path()).exists());
    }
}
