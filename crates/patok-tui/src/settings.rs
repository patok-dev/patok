//! The shell's central tui-schema settings: loaded
//! once, applied and persisted by the shell itself -- no round trip through the engine,
//! since tui-schema settings are not build-loop state -- and reloaded when a config file
//! changes on disk. Rendering consumes the values in the settings overlay (T15.1).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use patok_core::config::{
    ConfigFiles, SettingValue, TuiSettings, load_tui, project_file, set_tui_field,
};
use patok_proto::settings_change::Change;
use patok_proto::{Provider, Role, RunMode};

/// The shell's own central in-memory configuration: the merged,
/// normalized tui settings plus the state needed to detect a hand-edited config file.
#[derive(Debug)]
pub struct ShellSettings {
    settings: TuiSettings,
    files: ConfigFiles,
    project_dir: PathBuf,
    /// The project's slot of the projects dir, where the project-local config file
    /// lives; `None` (no `XDG_DATA_HOME`/`HOME`) skips the project layer.
    data_dir: Option<PathBuf>,
    /// Last-seen modification times of the config files, in layer order.
    mtimes: Vec<Option<SystemTime>>,
}

impl ShellSettings {
    /// Loads the tui schema across the layers from the process environment; the warnings
    /// are the caller's to show.
    pub fn load(project_dir: &Path) -> (Self, Vec<String>) {
        Self::with_files(
            ConfigFiles::from_env(),
            project_dir,
            patok_core::paths::project_data_dir_from_env(project_dir),
        )
    }

    /// [`ShellSettings::load`] with explicit user layers and data dir (testable).
    pub fn with_files(
        files: ConfigFiles,
        project_dir: &Path,
        data_dir: Option<PathBuf>,
    ) -> (Self, Vec<String>) {
        let (settings, warnings) = load_tui(&files, project_dir, data_dir.as_deref());
        let mtimes = mtimes(&files, data_dir.as_deref());
        (
            Self {
                settings,
                files,
                project_dir: project_dir.to_path_buf(),
                data_dir,
                mtimes,
            },
            warnings,
        )
    }

    /// Applies one tui-schema settings change: validated exactly as an on-disk field,
    /// persisted to the user-local `config.local.toml` (the resolved layer of every tui
    /// field), and effective immediately -- the tui schema has no unit-of-work boundary.
    /// Returns the persistence and reload warnings; errors are user-facing rejection
    /// reasons.
    pub fn apply(&mut self, field: &str, value: SettingValue) -> Result<Vec<String>, String> {
        let mut warnings = Vec::new();
        set_tui_field(&self.files, field, value, &mut warnings).map_err(|e| e.message)?;
        let (_, reload_warnings) = self.remerge();
        warnings.extend(reload_warnings);
        Ok(warnings)
    }

    /// Reloads when a config file changed on disk: the same re-merge and re-normalization as at startup, field by field,
    /// with the same fallback warnings. Returns the warnings when a reload happened,
    /// `None` when nothing changed.
    pub fn reload_if_changed(&mut self) -> Option<Vec<String>> {
        let now = mtimes(&self.files, self.data_dir.as_deref());
        if now == self.mtimes {
            return None;
        }
        self.mtimes = now;
        let (_, warnings) = self.remerge();
        Some(warnings)
    }

    /// Reloads all layers unconditionally (the shell itself just rewrote a file, so the
    /// mtime-based reload cannot be relied on); returns the re-merge warnings.
    pub fn reload(&mut self) -> Vec<String> {
        let (_, warnings) = self.remerge();
        warnings
    }

    /// Re-merges all layers into the in-memory settings, refreshing the stored mtimes.
    fn remerge(&mut self) -> (TuiSettings, Vec<String>) {
        let (settings, warnings) =
            load_tui(&self.files, &self.project_dir, self.data_dir.as_deref());
        self.settings = settings.clone();
        // T15.1's overlay: a field the operator is mid-edit on is left alone by this
        // reconciliation until the edit is committed or discarded.
        self.mtimes = mtimes(&self.files, self.data_dir.as_deref());
        (settings, warnings)
    }

    pub fn settings(&self) -> &TuiSettings {
        &self.settings
    }

    /// The config files the shell reads, with the user-local layer every tui field is
    /// persisted to (used by the settings overlay's touched-file restore).
    pub fn files(&self) -> &ConfigFiles {
        &self.files
    }
}

/// The modification times of the config files the shell reads, in layer order.
fn mtimes(files: &ConfigFiles, data_dir: Option<&Path>) -> Vec<Option<SystemTime>> {
    [
        files.user_global.as_deref(),
        files.user_local.as_deref(),
        data_dir.map(project_file).as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok())
    .collect()
}

/// Maps one daemon-schema settings change onto its typed wire case, the exact inverse of
/// the engine's `from_proto` (crates/patok-engine/src/settings.rs): every overlay row a
/// shell can commit has a case here; names without one are a programming error on the
/// shell side. The value's shape is checked against the case, not just the registry, so
/// a mismatch is a user-facing rejection rather than a wire corruption.
pub fn to_proto(field: &str, value: SettingValue) -> Result<Change, String> {
    let wrong = |expect: &str| Err(format!("`{field}` must be {expect}"));
    let boolean = || -> Result<bool, String> {
        value
            .as_bool()
            .ok_or_else(|| format!("`{field}` must be a boolean"))
    };
    let uint = || -> Result<u32, String> {
        let raw = value
            .as_uint()
            .ok_or_else(|| format!("`{field}` must be a whole number"))?;
        u32::try_from(raw).map_err(|_| format!("`{field}` must be at most {}", u32::MAX))
    };
    let string = || -> Result<String, String> {
        value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| format!("`{field}` must be a string"))
    };
    Ok(match field {
        "provider" => Change::Provider(provider_of(&string()?)? as i32),
        "model" => Change::ModelOverride(string()?),
        "research_provider" | "planner_provider" | "builder_provider" | "reviewer_provider"
        | "discovery_provider" => Change::RoleProvider(patok_proto::RoleProvider {
            role: role_of(&field[..field.len() - 9])? as i32,
            provider: provider_of(&string()?)? as i32,
        }),
        "research_model" | "planner_model" | "builder_model" | "reviewer_model"
        | "discovery_model" => Change::RoleModel(patok_proto::RoleModel {
            role: role_of(&field[..field.len() - 6])? as i32,
            model: string()?,
        }),
        "run_mode" => Change::RunMode(match string()?.as_str() {
            "sprint" => RunMode::Sprint,
            "continuous" => RunMode::Continuous,
            _ => return wrong("one of `sprint`, `continuous`"),
        } as i32),
        "plan_enabled" => Change::PlanEnabled(boolean()?),
        "skip_planner_for_simple" => Change::SkipPlannerForSimple(boolean()?),
        "skip_research_for_simple" => Change::SkipResearchForSimple(boolean()?),
        "skip_review_for_simple" => Change::SkipReviewForSimple(boolean()?),
        "review_confidence_threshold" => Change::ReviewConfidenceThreshold(uint()?),
        "review_multipass_threshold" => Change::ReviewMultipassThreshold(uint()?),
        "batch_review" => Change::BatchReview(boolean()?),
        "planner_lookahead" => Change::PlannerLookahead(boolean()?),
        "review_in_loop" => Change::ReviewInLoop(boolean()?),
        "confidence_threshold" => Change::ConfidenceThreshold(match value {
            SettingValue::Float(f) => f,
            _ => return wrong("a number"),
        }),
        "agent_timeout_secs" => Change::AgentTimeoutSecs(uint()?),
        "pause_between_tasks_secs" => Change::PauseBetweenTasksSecs(uint()?),
        "pause_between_agents_secs" => Change::PauseBetweenAgentsSecs(uint()?),
        "pause_between_cycles_secs" => Change::PauseBetweenCyclesSecs(uint()?),
        "engine_idle_timeout_secs" => Change::EngineIdleTimeoutSecs(uint()?),
        "adaptive_pauses" => Change::AdaptivePauses(boolean()?),
        "discovery_cooldown_secs" => Change::DiscoveryCooldownSecs(uint()?),
        "discovery_cooldown_cap_secs" => Change::DiscoveryCooldownCapSecs(uint()?),
        "auto_push_remote" => Change::AutoPushRemote(string()?),
        other => return Err(format!("no settings-change case for `{other}`")),
    })
}

fn role_of(name: &str) -> Result<Role, String> {
    match name {
        "research" => Ok(Role::Research),
        "planner" => Ok(Role::Planner),
        "builder" => Ok(Role::Builder),
        "reviewer" => Ok(Role::Reviewer),
        "discovery" => Ok(Role::Discovery),
        _ => Err(format!("unknown agent role `{name}`")),
    }
}

/// The canonical wire spelling of a provider; unknown text is a user-facing rejection.
fn provider_of(name: &str) -> Result<Provider, String> {
    match name {
        "claude" => Ok(Provider::Claude),
        "codex" => Ok(Provider::Codex),
        "opencode" => Ok(Provider::Opencode),
        "ghcopilot" => Ok(Provider::Ghcopilot),
        "mistral" => Ok(Provider::Mistral),
        other => Err(format!("`provider` must be a provider name, got {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patok_core::config::{RailMode, Theme};
    use patok_proto::settings_change::Change;

    /// The inverse of the engine's `from_proto`: every case this shell can produce maps
    /// back onto its registry key, so the two ends agree at compile time.
    #[test]
    fn to_proto_maps_every_overlay_field_onto_its_wire_case() {
        let str = |v: &str| SettingValue::Str(v.into());
        let uint = |v: u64| SettingValue::Uint(v);
        assert_eq!(
            to_proto("provider", str("mistral")).unwrap(),
            Change::Provider(Provider::Mistral as i32)
        );
        assert_eq!(
            to_proto("model", str("opus")).unwrap(),
            Change::ModelOverride("opus".into())
        );
        // The per-role provider and model rows (T90.1) map onto their wire cases.
        for (role, model) in [
            ("research", "sonnet"),
            ("planner", "opus"),
            ("builder", "opus"),
            ("reviewer", "sonnet"),
            ("discovery", "opus"),
        ] {
            assert_eq!(
                to_proto(&format!("{role}_model"), str(model)).unwrap(),
                Change::RoleModel(patok_proto::RoleModel {
                    role: role_of(role).unwrap() as i32,
                    model: model.into(),
                }),
                "{role}_model"
            );
            assert_eq!(
                to_proto(&format!("{role}_provider"), str("mistral")).unwrap(),
                Change::RoleProvider(patok_proto::RoleProvider {
                    role: role_of(role).unwrap() as i32,
                    provider: Provider::Mistral as i32,
                }),
                "{role}_provider"
            );
        }
        assert_eq!(
            to_proto("run_mode", str("continuous")).unwrap(),
            Change::RunMode(RunMode::Continuous as i32)
        );
        for (field, change) in [
            ("plan_enabled", Change::PlanEnabled(false)),
            (
                "skip_planner_for_simple",
                Change::SkipPlannerForSimple(true),
            ),
            (
                "skip_research_for_simple",
                Change::SkipResearchForSimple(true),
            ),
            ("skip_review_for_simple", Change::SkipReviewForSimple(true)),
            ("batch_review", Change::BatchReview(false)),
            ("planner_lookahead", Change::PlannerLookahead(true)),
            ("review_in_loop", Change::ReviewInLoop(true)),
            ("adaptive_pauses", Change::AdaptivePauses(false)),
        ] {
            assert_eq!(
                to_proto(field, SettingValue::Bool(bool_of(&change))).unwrap(),
                change,
                "{field}"
            );
        }
        assert_eq!(
            to_proto("confidence_threshold", SettingValue::Float(0.75)).unwrap(),
            Change::ConfidenceThreshold(0.75)
        );
        assert_eq!(
            to_proto("review_confidence_threshold", uint(5)).unwrap(),
            Change::ReviewConfidenceThreshold(5)
        );
        assert_eq!(
            to_proto("review_multipass_threshold", uint(8)).unwrap(),
            Change::ReviewMultipassThreshold(8)
        );
        assert_eq!(
            to_proto("agent_timeout_secs", uint(120)).unwrap(),
            Change::AgentTimeoutSecs(120)
        );
        assert_eq!(
            to_proto("pause_between_tasks_secs", uint(10)).unwrap(),
            Change::PauseBetweenTasksSecs(10)
        );
        assert_eq!(
            to_proto("pause_between_agents_secs", uint(3)).unwrap(),
            Change::PauseBetweenAgentsSecs(3)
        );
        assert_eq!(
            to_proto("pause_between_cycles_secs", uint(30)).unwrap(),
            Change::PauseBetweenCyclesSecs(30)
        );
        assert_eq!(
            to_proto("engine_idle_timeout_secs", uint(1800)).unwrap(),
            Change::EngineIdleTimeoutSecs(1800)
        );
        assert_eq!(
            to_proto("discovery_cooldown_secs", uint(300)).unwrap(),
            Change::DiscoveryCooldownSecs(300)
        );
        assert_eq!(
            to_proto("discovery_cooldown_cap_secs", uint(1800)).unwrap(),
            Change::DiscoveryCooldownCapSecs(1800)
        );
        assert_eq!(
            to_proto("auto_push_remote", str("origin")).unwrap(),
            Change::AutoPushRemote("origin".into())
        );
        // An empty optional string clears the key.
        assert_eq!(
            to_proto("auto_push_remote", str("")).unwrap(),
            Change::AutoPushRemote(String::new())
        );
    }

    /// The boolean carried by a `Change` case, for the round-trip assertion above.
    fn bool_of(change: &Change) -> bool {
        match change {
            Change::PlanEnabled(v)
            | Change::SkipPlannerForSimple(v)
            | Change::SkipResearchForSimple(v)
            | Change::SkipReviewForSimple(v)
            | Change::BatchReview(v)
            | Change::PlannerLookahead(v)
            | Change::ReviewInLoop(v)
            | Change::AdaptivePauses(v) => *v,
            _ => panic!("not a boolean case"),
        }
    }

    #[test]
    fn to_proto_rejects_mismatched_shapes_and_unknown_fields() {
        let error = to_proto("plan_enabled", SettingValue::Str("yes".into())).unwrap_err();
        assert!(error.contains("must be a boolean"), "{error}");
        let error = to_proto("agent_timeout_secs", SettingValue::Str("x".into())).unwrap_err();
        assert!(error.contains("must be a whole number"), "{error}");
        // Wire numbers are u32: anything larger is rejected before it can wrap.
        let error = to_proto(
            "agent_timeout_secs",
            SettingValue::Uint(u64::from(u32::MAX) + 1),
        )
        .unwrap_err();
        assert!(error.contains("at most"), "{error}");
        let error = to_proto("run_mode", SettingValue::Str("fast".into())).unwrap_err();
        assert!(error.contains("`sprint`, `continuous`"), "{error}");
        let error = to_proto("no_such_key", SettingValue::Uint(1)).unwrap_err();
        assert!(error.contains("no settings-change case"), "{error}");
        let error = to_proto("provider", SettingValue::Str("not-a-provider".into())).unwrap_err();
        assert!(error.contains("provider name"), "{error}");
        let error = to_proto("research_model", SettingValue::Bool(true)).unwrap_err();
        assert!(error.contains("must be a string"), "{error}");
    }

    /// A holder whose user-local layer is a tempdir file, plus its path.
    fn holder() -> (tempfile::TempDir, ShellSettings) {
        let dir = tempfile::tempdir().unwrap();
        let files = ConfigFiles {
            user_global: None,
            user_local: Some(dir.path().join("config.local.toml")),
        };
        let (settings, warnings) = ShellSettings::with_files(files, dir.path(), None);
        assert!(warnings.is_empty());
        (dir, settings)
    }

    fn user_local(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("config.local.toml")).unwrap()
    }

    #[test]
    fn a_valid_change_updates_memory_and_persists() {
        let (dir, mut shell) = holder();
        shell
            .apply("theme", SettingValue::Str("solarized_dark".into()))
            .unwrap();
        assert_eq!(shell.settings().theme, Theme::SolarizedDark);
        shell
            .apply("agent_pane_split", SettingValue::Uint(70))
            .unwrap();
        assert_eq!(shell.settings().agent_pane_split, 70);
        let text = user_local(&dir);
        assert!(text.contains("theme = \"solarized_dark\""), "{text}");
        assert!(text.contains("agent_pane_split = 70"), "{text}");
    }

    /// A rail-mode change (T57.1) persists to the user-local layer like every
    /// tui field, so the choice survives a restart; since T58.1 the fresh
    /// default is `normal`, and an explicit `compact` still wins.
    #[test]
    fn a_rail_mode_change_updates_memory_and_persists() {
        let (dir, mut shell) = holder();
        // A fresh config resolves the rail mode to the default, `normal`.
        assert_eq!(shell.settings().rail_mode, RailMode::Normal);
        shell
            .apply("rail_mode", SettingValue::Str("compact".into()))
            .unwrap();
        assert_eq!(shell.settings().rail_mode, RailMode::Compact);
        let text = user_local(&dir);
        assert!(text.contains("rail_mode = \"compact\""), "{text}");
        assert!(text.contains("[tui]"), "{text}");

        // A restart over the same layers still resolves the explicit compact.
        let files = ConfigFiles {
            user_global: None,
            user_local: Some(dir.path().join("config.local.toml")),
        };
        let (restarted, warnings) = ShellSettings::with_files(files, dir.path(), None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(restarted.settings().rail_mode, RailMode::Compact);

        // And back to normal.
        shell
            .apply("rail_mode", SettingValue::Str("normal".into()))
            .unwrap();
        assert_eq!(shell.settings().rail_mode, RailMode::Normal);

        // The detailed mode (T89.1) persists the same way.
        shell
            .apply("rail_mode", SettingValue::Str("detailed".into()))
            .unwrap();
        assert_eq!(shell.settings().rail_mode, RailMode::Detailed);
        let text = user_local(&dir);
        assert!(text.contains("rail_mode = \"detailed\""), "{text}");
    }

    /// A hand-edited `rail_mode` in the config file is picked up by the
    /// reload, the config-file switch path of the rail mode (T57.1): an
    /// explicit `compact` is honored over the `normal` default (T58.1).
    #[test]
    fn a_hand_edited_rail_mode_is_picked_up_by_the_reload() {
        let (dir, mut shell) = holder();
        assert!(shell.reload_if_changed().is_none());
        std::fs::write(
            dir.path().join("config.local.toml"),
            "[tui]\nrail_mode = \"compact\"\n",
        )
        .unwrap();
        let warnings = shell.reload_if_changed().expect("the change was picked up");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(shell.settings().rail_mode, RailMode::Compact);
        // Nothing changed since: no reload.
        assert!(shell.reload_if_changed().is_none());
    }

    /// An invalid `rail_mode` in a config layer falls back to `normal` (the
    /// default since T58.1) with a warning naming the field and the source
    /// file, keeping the file's other fields; an invalid submitted change is
    /// rejected without touching the file (T57.1).
    #[test]
    fn an_invalid_rail_mode_falls_back_to_normal_with_a_warning() {
        let (dir, mut shell) = holder();
        let path = dir.path().join("config.local.toml");
        std::fs::write(
            &path,
            "[tui]\ntheme = \"solarized_dark\"\nrail_mode = \"fancy\"\n",
        )
        .unwrap();
        let warnings = shell.reload_if_changed().expect("the change was picked up");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("tui.rail_mode must be one of"),
            "{warnings:?}"
        );
        assert!(
            warnings[0].contains(path.to_string_lossy().as_ref()),
            "{warnings:?}"
        );
        assert!(warnings[0].contains("fancy"), "{warnings:?}");
        assert_eq!(shell.settings().rail_mode, RailMode::Normal);
        assert_eq!(shell.settings().theme, Theme::SolarizedDark);

        // An invalid submitted value is rejected without touching the file.
        let before = std::fs::read_to_string(&path).unwrap();
        let error = shell
            .apply("rail_mode", SettingValue::Str("fancy".into()))
            .unwrap_err();
        assert!(error.contains("must be one of"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(shell.settings().rail_mode, RailMode::Normal);
    }

    #[test]
    fn an_invalid_or_unknown_change_is_rejected_without_touching_anything() {
        let (dir, mut shell) = holder();
        let error = shell
            .apply("theme", SettingValue::Str("neon".into()))
            .unwrap_err();
        assert!(error.contains("must be one of"), "{error}");
        let error = shell
            .apply("agent_pane_split", SettingValue::Uint(10))
            .unwrap_err();
        assert!(error.contains("between 20 and 80"), "{error}");
        let error = shell
            .apply("no_such_key", SettingValue::Uint(1))
            .unwrap_err();
        assert!(error.contains("unknown tui setting"), "{error}");
        assert!(
            !dir.path().join("config.local.toml").exists(),
            "nothing was written"
        );
        assert_eq!(shell.settings().theme, Theme::Dark);
    }

    #[test]
    fn a_hand_edited_file_is_picked_up_by_the_reload() {
        let (dir, mut shell) = holder();
        assert!(shell.reload_if_changed().is_none());
        std::fs::write(
            dir.path().join("config.local.toml"),
            "[tui]\ntheme = \"catppuccin_mocha\"\n",
        )
        .unwrap();
        let warnings = shell.reload_if_changed().expect("the change was picked up");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(shell.settings().theme, Theme::CatppuccinMocha);
        // Nothing changed since: no reload.
        assert!(shell.reload_if_changed().is_none());

        // A wrong-typed field falls back to its default with a warning; the other
        // fields of the same file are kept.
        std::fs::write(
            dir.path().join("config.local.toml"),
            "[tui]\ntheme = \"solarized_dark\"\npreview_wrap = \"yes\"\n",
        )
        .unwrap();
        let warnings = shell.reload_if_changed().expect("the change was picked up");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("preview_wrap"), "{warnings:?}");
        assert_eq!(shell.settings().theme, Theme::SolarizedDark);
        assert!(shell.settings().preview_wrap);
    }

    /// An invalid theme name in a config layer falls back to the default theme
    /// with a warning naming the field and the source file, so the app stays
    /// usable; this includes the pre-T36.1 placeholder names.
    #[test]
    fn an_invalid_theme_name_falls_back_with_a_warning() {
        let (dir, mut shell) = holder();
        let path = dir.path().join("config.local.toml");
        std::fs::write(&path, "[tui]\ntheme = \"neon\"\n").unwrap();
        let warnings = shell.reload_if_changed().expect("the change was picked up");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("tui.theme must be one of"),
            "{warnings:?}"
        );
        assert!(
            warnings[0].contains(path.to_string_lossy().as_ref()),
            "{warnings:?}"
        );
        assert!(warnings[0].contains("neon"), "{warnings:?}");
        assert_eq!(shell.settings().theme, Theme::Dark);

        // The old placeholder spelling is no longer accepted either.
        std::fs::write(&path, "[tui]\ntheme = \"catppuccin\"\n").unwrap();
        let warnings = shell.reload_if_changed().expect("the change was picked up");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(shell.settings().theme, Theme::Dark);
    }
}
