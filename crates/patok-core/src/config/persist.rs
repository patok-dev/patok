//! Persistence of a settings change: read-modify-write on the
//! field's resolved layer file, touching only the `[daemon]` or `[tui]` table the field
//! belongs to so the rest of the file survives. Daemon fields resolve to the
//! project-local `patok.config.toml` (in the project's slot of the projects dir); tui
//! fields resolve to the user-local `config.local.toml`. Writes validate types and
//! reject unknown field names;
//! optional-string fields written as the empty string become null (the key is removed).

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::daemon::DaemonSettings;
use super::{ConfigFiles, project_file};

/// A settings value as submitted by a settings change, mapped onto the field's expected
/// type by the per-schema field registry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SettingValue {
    Bool(bool),
    Uint(u64),
    Float(f64),
    Str(String),
    /// Remove the key from its resolved layer file, so the lower layers (and finally
    /// the default) apply again. The only field that needs it is `tui.truecolor`, whose
    /// "auto" state is the absence of the key; the engine never reports it.
    Unset,
}

impl SettingValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_uint(&self) -> Option<u64> {
        match self {
            Self::Uint(u) => Some(*u),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }
}

impl fmt::Display for SettingValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(b) => write!(f, "{b}"),
            Self::Uint(u) => write!(f, "{u}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Str(s) => write!(f, "{s}"),
            Self::Unset => write!(f, "(not set)"),
        }
    }
}

/// When a settings change takes effect: a field
/// that parameterizes a unit of work (a build-loop start, a task, an agent session) keeps
/// the value that unit started with and takes effect at the next such start; a field read
/// at the point of use applies immediately.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyTiming {
    Immediate,
    NextUnitOfWork,
}

/// A persistence failure; `message` is user-facing.
#[derive(Debug)]
pub struct PersistError {
    pub message: String,
}

impl PersistError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for PersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PersistError {}

/// What a field accepts.
enum FieldKind {
    Bool,
    Uint,
    /// An unsigned integer that must be greater than zero, validated exactly like the
    /// on-disk parser of the same field ("a whole number of seconds greater than zero").
    PositiveUint,
    Port,
    Float,
    /// A non-optional string; an empty string is rejected.
    Str,
    /// An optional string; the empty string removes the key so the default applies.
    OptStr,
    /// An enumerated field and its accepted spellings (trimmed, lowercased).
    Enum(&'static [&'static str]),
    /// An unsigned integer with an inclusive range.
    Range(u64, u64),
    /// A known key of the other (composite) shape that the settings-change flow cannot
    /// write as a single value.
    Composite,
}

const PROVIDERS: &[&str] = &[
    "claude",
    "codex",
    "opencode",
    "ghcopilot",
    "gh-copilot",
    "github-copilot",
    "copilot",
    "mistral",
    "vibe",
];

/// The daemon field registry: every `[daemon]` key and what it accepts.
fn daemon_field(name: &str) -> Option<FieldKind> {
    Some(match name {
        "provider" => FieldKind::Enum(PROVIDERS),
        "model"
        | "local_model"
        | "build_command"
        | "auto_push_remote"
        | "on_task_complete_hook"
        | "history_dir" => FieldKind::OptStr,
        "research_model"
        | "planner_model"
        | "builder_model"
        | "reviewer_model"
        | "discovery_model"
        | "learning_extraction_model"
        | "embedding_model" => FieldKind::Str,
        "research_provider"
        | "planner_provider"
        | "builder_provider"
        | "reviewer_provider"
        | "discovery_provider"
        | "learning_extraction_provider" => FieldKind::Enum(PROVIDERS),
        "model_catalog_refresh_secs"
        | "review_confidence_threshold"
        | "plan_revision_cycles"
        | "review_multipass_threshold"
        | "planning_iterations"
        | "pause_between_tasks_secs"
        | "pause_between_agents_secs"
        | "pause_between_cycles_secs"
        | "engine_idle_timeout_secs"
        | "max_learning_injection"
        | "min_learning_injection"
        | "history_search_results"
        | "history_retention_tasks"
        | "observatory_retention_days"
        | "embedding_timeout_ms" => FieldKind::Uint,
        "agent_timeout_secs" | "discovery_cooldown_secs" | "discovery_cooldown_cap_secs" => {
            FieldKind::PositiveUint
        }
        "run_mode" => FieldKind::Enum(&["sprint", "continuous"]),
        "plan_revision_accept_policy" => {
            FieldKind::Enum(&["no-high", "no-high-medium", "no-findings"])
        }
        "confidence_threshold" => FieldKind::Float,
        "embedding_port" => FieldKind::Port,
        "skip_planner_for_simple"
        | "skip_research_for_simple"
        | "skip_review_for_simple"
        | "batch_review"
        | "planner_lookahead"
        | "review_in_loop"
        | "semgrep_enabled"
        | "require_human_approval"
        | "plan_enabled"
        | "adaptive_pauses"
        | "phase_isolation"
        | "enforce_phase_rbac"
        | "semantic_match_enabled"
        | "show_retrieval_panel" => FieldKind::Bool,
        "stages" | "catalog_url_overrides" | "stage_overrides" | "semgrep_rulesets" | "plugins" => {
            FieldKind::Composite
        }
        _ => return None,
    })
}

/// The tui field registry: every `[tui]` key and what it accepts.
fn tui_field(name: &str) -> Option<FieldKind> {
    Some(match name {
        "theme" => FieldKind::Enum(super::tui::THEME_KEYS),
        "truecolor" | "preview_wrap" => FieldKind::Bool,
        "agent_pane_split" => FieldKind::Range(
            super::tui::AGENT_PANE_SPLIT_MIN,
            super::tui::AGENT_PANE_SPLIT_MAX,
        ),
        "update_channel" => FieldKind::Enum(&["stable", "dev"]),
        "rail_mode" => FieldKind::Enum(&["compact", "normal", "detailed"]),
        _ => return None,
    })
}

/// Every non-composite `[daemon]` key, in registry order: exactly the fields the
/// settings-change flow can carry as a single value. Composite keys (`stages`,
/// `catalog_url_overrides`, `stage_overrides`, `semgrep_rulesets`, `plugins`) have no
/// single-value shape, so the diff skips them.
const DAEMON_FIELDS: &[&str] = &[
    // Routing (section 4).
    "provider",
    "model",
    "local_model",
    "model_catalog_refresh_secs",
    "research_model",
    "planner_model",
    "builder_model",
    "reviewer_model",
    "discovery_model",
    "learning_extraction_model",
    "research_provider",
    "planner_provider",
    "builder_provider",
    "reviewer_provider",
    "discovery_provider",
    "learning_extraction_provider",
    // Pipeline flow (section 5).
    "run_mode",
    "skip_planner_for_simple",
    "skip_research_for_simple",
    "skip_review_for_simple",
    "review_confidence_threshold",
    "batch_review",
    "planner_lookahead",
    "plan_revision_cycles",
    "plan_revision_accept_policy",
    "review_in_loop",
    "review_multipass_threshold",
    "confidence_threshold",
    "semgrep_enabled",
    "build_command",
    "planning_iterations",
    "require_human_approval",
    "auto_push_remote",
    "on_task_complete_hook",
    "discovery_cooldown_secs",
    "discovery_cooldown_cap_secs",
    "plan_enabled",
    // Timing (section 6).
    "agent_timeout_secs",
    "pause_between_tasks_secs",
    "pause_between_agents_secs",
    "pause_between_cycles_secs",
    "adaptive_pauses",
    "engine_idle_timeout_secs",
    // Isolation and security (section 7).
    "phase_isolation",
    "enforce_phase_rbac",
    // Learnings, history and embeddings (section 8).
    "history_dir",
    "max_learning_injection",
    "min_learning_injection",
    "history_search_results",
    "history_retention_tasks",
    "observatory_retention_days",
    "semantic_match_enabled",
    "embedding_model",
    "embedding_port",
    "embedding_timeout_ms",
    "show_retrieval_panel",
];

/// Daemon fields read at the point of use, so a change applies immediately: everything else parameterizes a unit of work and takes
/// effect at the next such start. A new field joins whichever list matches how it is
/// actually consumed.
const IMMEDIATE_DAEMON_FIELDS: &[&str] = &[
    "observatory_retention_days",
    "auto_push_remote",
    "on_task_complete_hook",
    "show_retrieval_panel",
];

/// When a change to `field` takes effect; `None` for a name
/// outside the daemon registry (including composite keys).
pub fn daemon_apply_timing(field: &str) -> Option<ApplyTiming> {
    if !DAEMON_FIELDS.contains(&field) {
        return None;
    }
    Some(if IMMEDIATE_DAEMON_FIELDS.contains(&field) {
        ApplyTiming::Immediate
    } else {
        ApplyTiming::NextUnitOfWork
    })
}

/// The field's effective (post-merge, post-normalization) value, as reported back with a
/// [`crate::config::ApplyTiming`] broadcast; `None` for a name outside the registry or a
/// composite key.
pub fn daemon_field_value(settings: &DaemonSettings, field: &str) -> Option<SettingValue> {
    let string = |value: Option<&str>| SettingValue::Str(value.unwrap_or_default().into());
    Some(match field {
        "provider" => SettingValue::Str(settings.provider.as_str().into()),
        "model" => string(settings.model.as_deref()),
        "local_model" => string(settings.local_model.as_deref()),
        "build_command" => string(settings.build_command.as_deref()),
        "auto_push_remote" => string(settings.auto_push_remote.as_deref()),
        "on_task_complete_hook" => string(settings.on_task_complete_hook.as_deref()),
        "history_dir" => string(settings.history_dir.as_deref()),
        "research_model" => SettingValue::Str(settings.research_model.clone()),
        "planner_model" => SettingValue::Str(settings.planner_model.clone()),
        "builder_model" => SettingValue::Str(settings.builder_model.clone()),
        "reviewer_model" => SettingValue::Str(settings.reviewer_model.clone()),
        "discovery_model" => SettingValue::Str(settings.discovery_model.clone()),
        "learning_extraction_model" => {
            SettingValue::Str(settings.learning_extraction_model.clone())
        }
        "research_provider" => SettingValue::Str(settings.research_provider.as_str().into()),
        "planner_provider" => SettingValue::Str(settings.planner_provider.as_str().into()),
        "builder_provider" => SettingValue::Str(settings.builder_provider.as_str().into()),
        "reviewer_provider" => SettingValue::Str(settings.reviewer_provider.as_str().into()),
        "discovery_provider" => SettingValue::Str(settings.discovery_provider.as_str().into()),
        "learning_extraction_provider" => {
            SettingValue::Str(settings.learning_extraction_provider.as_str().into())
        }
        "run_mode" => SettingValue::Str(settings.run_mode.as_str().into()),
        "plan_revision_accept_policy" => {
            SettingValue::Str(settings.plan_revision_accept_policy.as_str().into())
        }
        "confidence_threshold" => SettingValue::Float(settings.confidence_threshold),
        "model_catalog_refresh_secs" => SettingValue::Uint(settings.model_catalog_refresh_secs),
        "review_confidence_threshold" => SettingValue::Uint(settings.review_confidence_threshold),
        "plan_revision_cycles" => SettingValue::Uint(settings.plan_revision_cycles),
        "review_multipass_threshold" => SettingValue::Uint(settings.review_multipass_threshold),
        "planning_iterations" => SettingValue::Uint(settings.planning_iterations),
        "discovery_cooldown_secs" => SettingValue::Uint(settings.discovery_cooldown_secs),
        "discovery_cooldown_cap_secs" => SettingValue::Uint(settings.discovery_cooldown_cap_secs),
        "agent_timeout_secs" => SettingValue::Uint(settings.agent_timeout_secs),
        "pause_between_tasks_secs" => SettingValue::Uint(settings.pause_between_tasks_secs),
        "pause_between_agents_secs" => SettingValue::Uint(settings.pause_between_agents_secs),
        "pause_between_cycles_secs" => SettingValue::Uint(settings.pause_between_cycles_secs),
        "engine_idle_timeout_secs" => SettingValue::Uint(settings.engine_idle_timeout_secs),
        "max_learning_injection" => SettingValue::Uint(settings.max_learning_injection),
        "min_learning_injection" => SettingValue::Uint(settings.min_learning_injection),
        "history_search_results" => SettingValue::Uint(settings.history_search_results),
        "history_retention_tasks" => SettingValue::Uint(settings.history_retention_tasks),
        "observatory_retention_days" => SettingValue::Uint(settings.observatory_retention_days),
        "embedding_port" => SettingValue::Uint(u64::from(settings.embedding_port)),
        "embedding_timeout_ms" => SettingValue::Uint(settings.embedding_timeout_ms),
        "embedding_model" => SettingValue::Str(settings.embedding_model.clone()),
        "skip_planner_for_simple" => SettingValue::Bool(settings.skip_planner_for_simple),
        "skip_research_for_simple" => SettingValue::Bool(settings.skip_research_for_simple),
        "skip_review_for_simple" => SettingValue::Bool(settings.skip_review_for_simple),
        "batch_review" => SettingValue::Bool(settings.batch_review),
        "planner_lookahead" => SettingValue::Bool(settings.planner_lookahead),
        "review_in_loop" => SettingValue::Bool(settings.review_in_loop),
        "semgrep_enabled" => SettingValue::Bool(settings.semgrep_enabled),
        "require_human_approval" => SettingValue::Bool(settings.require_human_approval),
        "plan_enabled" => SettingValue::Bool(settings.plan_enabled),
        "adaptive_pauses" => SettingValue::Bool(settings.adaptive_pauses),
        "phase_isolation" => SettingValue::Bool(settings.phase_isolation),
        "enforce_phase_rbac" => SettingValue::Bool(settings.enforce_phase_rbac),
        "semantic_match_enabled" => SettingValue::Bool(settings.semantic_match_enabled),
        "show_retrieval_panel" => SettingValue::Bool(settings.show_retrieval_panel),
        _ => return None,
    })
}

/// The registry fields whose effective value differs between two settings, in registry
/// order -- the broadcast of one settings swap, one event per changed field.
pub fn daemon_changed_fields(old: &DaemonSettings, new: &DaemonSettings) -> Vec<String> {
    DAEMON_FIELDS
        .iter()
        .filter(|field| daemon_field_value(old, field) != daemon_field_value(new, field))
        .map(|field| (*field).to_string())
        .collect()
}

/// Every registered daemon field with its effective (post-merge, post-normalization)
/// value, in registry order -- the engine-reported readout the settings overlay renders
/// its rows from (T15.1).
pub fn daemon_readout(
    settings: &DaemonSettings,
) -> std::collections::BTreeMap<String, SettingValue> {
    DAEMON_FIELDS
        .iter()
        .filter_map(|field| {
            daemon_field_value(settings, field).map(|value| ((*field).to_string(), value))
        })
        .collect()
}

/// Validates `value` against `kind`, mapping it onto the TOML value to store; `None`
/// means the key must be removed. `schema` and `name` only appear in error messages.
fn validate(
    schema: &str,
    name: &str,
    kind: &FieldKind,
    value: SettingValue,
) -> Result<Option<toml::Value>, PersistError> {
    let got = value.type_name();
    let reject = |expect: &str| {
        Err(PersistError::new(format!(
            "{schema}.{name} must be {expect}, got {got}"
        )))
    };
    match (kind, value) {
        (FieldKind::Composite, _) => Err(PersistError::new(format!(
            "{schema}.{name} cannot be changed as a single value through the settings change flow"
        ))),
        // Unset is accepted for every registered field: it removes the key.
        (_, SettingValue::Unset) => Ok(None),
        (FieldKind::Bool, SettingValue::Bool(b)) => Ok(Some(b.into())),
        (FieldKind::Bool, _) => reject("a boolean"),
        (FieldKind::Uint, SettingValue::Uint(u)) => integer(u).map(Some).map_err(|_| {
            PersistError::new(format!("{schema}.{name} must be a whole number, got {u}"))
        }),
        (FieldKind::Uint, _) => reject("a whole number"),
        (FieldKind::PositiveUint, SettingValue::Uint(u)) if u > 0 => {
            integer(u).map(Some).map_err(|_| {
                PersistError::new(format!("{schema}.{name} must be a whole number, got {u}"))
            })
        }
        (FieldKind::PositiveUint, _) => reject("a whole number of seconds greater than zero"),
        (FieldKind::Port, SettingValue::Uint(p)) if (1..=u16::MAX as u64).contains(&p) => {
            integer(p).map(Some).map_err(|_| {
                PersistError::new(format!("{schema}.{name} must be a port number (1-65535)"))
            })
        }
        (FieldKind::Port, _) => Err(PersistError::new(format!(
            "{schema}.{name} must be a port number (1-65535), got {got}"
        ))),
        (FieldKind::Float, SettingValue::Float(f)) => Ok(Some(f.into())),
        (FieldKind::Float, _) => reject("a number"),
        (FieldKind::Str, SettingValue::Str(s)) if !s.trim().is_empty() => Ok(Some(s.into())),
        (FieldKind::Str, _) => Err(PersistError::new(format!(
            "{schema}.{name} must be a non-empty string, got {got}"
        ))),
        (FieldKind::OptStr, SettingValue::Str(s)) if s.trim().is_empty() => Ok(None),
        (FieldKind::OptStr, SettingValue::Str(s)) => Ok(Some(s.into())),
        (FieldKind::OptStr, _) => reject("a string"),
        (FieldKind::Range(min, max), SettingValue::Uint(u)) if (*min..=*max).contains(&u) => {
            integer(u).map(Some).map_err(|_| {
                PersistError::new(format!("{schema}.{name} must be between {min} and {max}"))
            })
        }
        (FieldKind::Range(min, max), SettingValue::Uint(u)) => Err(PersistError::new(format!(
            "{schema}.{name} must be between {min} and {max}, got {u}"
        ))),
        (FieldKind::Range(..), _) => reject("a whole number"),
        (FieldKind::Enum(accepted), SettingValue::Str(s)) => {
            let normalized = s.trim().to_lowercase();
            if accepted.contains(&normalized.as_str()) {
                Ok(Some(normalized.into()))
            } else {
                Err(PersistError::new(format!(
                    "{schema}.{name} must be one of {}, got {s:?}",
                    accepted
                        .iter()
                        .map(|a| format!("`{a}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            }
        }
        (FieldKind::Enum(..), _) => reject("one of the documented values"),
    }
}

impl SettingValue {
    fn type_name(&self) -> String {
        match self {
            Self::Bool(_) => "a boolean".into(),
            Self::Uint(_) => "a whole number".into(),
            Self::Float(_) => "a number".into(),
            Self::Str(_) => "a string".into(),
            Self::Unset => "unset".into(),
        }
    }
}

/// A non-negative whole number as a TOML value; the checked fields never exceed i64.
fn integer(u: u64) -> Result<toml::Value, std::num::TryFromIntError> {
    Ok(toml::Value::Integer(i64::try_from(u)?))
}

/// Writes `value` to the daemon field `name` in the project-local `patok.config.toml`
/// (the resolved layer of every daemon-schema field, inside the project's slot of the
/// projects dir), touching only its `[daemon]`
/// table. `warnings` receives the warning when an invalid target file is replaced.
pub fn set_daemon_field(
    data_dir: &Path,
    name: &str,
    value: SettingValue,
    warnings: &mut Vec<String>,
) -> Result<(), PersistError> {
    let kind = daemon_field(name)
        .ok_or_else(|| PersistError::new(format!("unknown daemon setting `{name}`")))?;
    let value = validate("daemon", name, &kind, value)?;
    write_field(&project_file(data_dir), "daemon", name, value, warnings)
}

/// Writes `value` to the tui field `name` in the user-local `config.local.toml` (the
/// resolved layer of every tui-schema field), touching only its `[tui]` table.
pub fn set_tui_field(
    files: &ConfigFiles,
    name: &str,
    value: SettingValue,
    warnings: &mut Vec<String>,
) -> Result<(), PersistError> {
    let kind = tui_field(name)
        .ok_or_else(|| PersistError::new(format!("unknown tui setting `{name}`")))?;
    let value = validate("tui", name, &kind, value)?;
    let target = files.user_local.clone().ok_or_else(|| {
        PersistError::new(
            "cannot resolve the user-local config file: neither XDG_CONFIG_HOME nor HOME is set",
        )
    })?;
    write_field(&target, "tui", name, value, warnings)
}

/// Read-modify-write on `path`: the existing document is parsed (an unreadable or
/// unparseable file is replaced, with a warning that the other settings in it are lost),
/// only `table`'s `name` key is set (or removed for `None`), and the document is written
/// back atomically.
fn write_field(
    path: &Path,
    table: &str,
    name: &str,
    value: Option<toml::Value>,
    warnings: &mut Vec<String>,
) -> Result<(), PersistError> {
    let mut doc: toml::Table = match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => toml::Table::new(),
        Ok(text) => match text.parse() {
            Ok(doc) => doc,
            Err(e) => {
                warnings.push(format!(
                    "replacing unparseable config {}: {}; every other setting in that file is lost",
                    path.display(),
                    e.to_string().trim_end()
                ));
                toml::Table::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => {
            warnings.push(format!(
                "replacing unreadable config {}: {e}; every other setting in that file is lost",
                path.display()
            ));
            toml::Table::new()
        }
    };
    let owned = doc
        .entry(table.to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let owned_table = match owned {
        toml::Value::Table(t) => t,
        other => {
            warnings.push(format!(
                "replacing the `{table}` table of {}: it must be a table, got {other}",
                path.display()
            ));
            *other = toml::Value::Table(toml::Table::new());
            other.as_table_mut().expect("just replaced with a table")
        }
    };
    match value {
        Some(value) => {
            owned_table.insert(name.to_string(), value);
        }
        None => {
            owned_table.remove(name);
        }
    }
    let text = toml::to_string(&doc)
        .map_err(|e| PersistError::new(format!("cannot serialize {}: {e}", path.display())))?;
    write_atomic(path, &text)
        .map_err(|e| PersistError::new(format!("cannot write {}: {e}", path.display())))
}

/// Writes through a temporary file in the same directory, then renames over the target
/// (same pattern as the task file).
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.to_path_buf();
    let unique = std::process::id()
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
    tmp.set_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config"),
        unique
    ));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderName;

    #[test]
    fn apply_timing_follows_the_registry() {
        // Point-of-use fields apply immediately.
        for field in [
            "observatory_retention_days",
            "auto_push_remote",
            "on_task_complete_hook",
            "show_retrieval_panel",
        ] {
            assert_eq!(
                daemon_apply_timing(field),
                Some(ApplyTiming::Immediate),
                "{field}"
            );
        }
        // Unit-of-work fields wait for the next start.
        for field in [
            "provider",
            "model",
            "run_mode",
            "plan_enabled",
            "agent_timeout_secs",
            "phase_isolation",
            "history_dir",
        ] {
            assert_eq!(
                daemon_apply_timing(field),
                Some(ApplyTiming::NextUnitOfWork),
                "{field}"
            );
        }
        // Unknown and composite names carry no timing.
        assert_eq!(daemon_apply_timing("stages"), None);
        assert_eq!(daemon_apply_timing("no_such_key"), None);
    }

    #[test]
    fn changed_fields_report_only_what_differs() {
        let mut new = DaemonSettings::default();
        assert!(daemon_changed_fields(&DaemonSettings::default(), &new).is_empty());
        new.agent_timeout_secs = 120;
        new.provider = ProviderName::Mistral;
        assert_eq!(
            daemon_changed_fields(&DaemonSettings::default(), &new),
            ["provider", "agent_timeout_secs"]
        );
        // The single-model override normalization shows through the readout: setting it
        // collapses the role models too, and the diff reports them.
        new.model = Some("mistral-small".into());
        assert_eq!(
            daemon_changed_fields(&DaemonSettings::default(), &new),
            ["provider", "model", "agent_timeout_secs"]
        );
        // A reload re-materializes first, so the override's collapsed role models count
        // as changed there; the readout reports the collapsed value.
        let materialized = {
            let raw = crate::config::daemon::DaemonLayer {
                model: Some("mistral-small".into()),
                ..Default::default()
            };
            crate::config::daemon::materialize(raw)
        };
        assert_eq!(
            daemon_changed_fields(&DaemonSettings::default(), &materialized),
            [
                "model",
                "research_model",
                "planner_model",
                "builder_model",
                "reviewer_model",
                "discovery_model",
                "learning_extraction_model",
            ]
        );
    }

    #[test]
    fn field_value_reports_effective_values() {
        let settings = DaemonSettings::default();
        assert_eq!(
            daemon_field_value(&settings, "provider").unwrap(),
            SettingValue::Str("claude".into())
        );
        assert_eq!(
            daemon_field_value(&settings, "run_mode").unwrap(),
            SettingValue::Str("sprint".into())
        );
        assert_eq!(
            daemon_field_value(&settings, "plan_revision_accept_policy").unwrap(),
            SettingValue::Str("no-high".into())
        );
        assert_eq!(
            daemon_field_value(&settings, "embedding_port").unwrap(),
            SettingValue::Uint(11434)
        );
        // An unset optional string reads back as cleared, not as a missing value.
        assert_eq!(
            daemon_field_value(&settings, "model").unwrap(),
            SettingValue::Str(String::new())
        );
        assert_eq!(daemon_field_value(&settings, "stages"), None);
        assert_eq!(daemon_field_value(&settings, "no_such_key"), None);
    }

    #[test]
    fn a_positive_uint_field_rejects_zero_like_the_disk_parser() {
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        for field in [
            "agent_timeout_secs",
            "discovery_cooldown_secs",
            "discovery_cooldown_cap_secs",
        ] {
            let error = set_daemon_field(dir.path(), field, SettingValue::Uint(0), &mut warnings)
                .unwrap_err();
            assert!(
                error.message.contains("greater than zero"),
                "{field}: {}",
                error.message
            );
            assert!(
                error.message.contains("must be a whole number"),
                "{field}: {}",
                error.message
            );
        }
        assert!(!dir.path().join("patok.config.toml").exists());
        set_daemon_field(
            dir.path(),
            "agent_timeout_secs",
            SettingValue::Uint(120),
            &mut warnings,
        )
        .unwrap();
    }

    #[test]
    fn unset_removes_the_key_and_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("patok.config.toml"),
            "[daemon]\nagent_timeout_secs = 120\nrun_mode = \"continuous\"\n",
        )
        .unwrap();
        let mut warnings = Vec::new();
        set_daemon_field(
            dir.path(),
            "agent_timeout_secs",
            SettingValue::Unset,
            &mut warnings,
        )
        .unwrap();
        let text = std::fs::read_to_string(dir.path().join("patok.config.toml")).unwrap();
        assert!(!text.contains("agent_timeout_secs"), "{text}");
        assert!(text.contains("run_mode = \"continuous\""), "{text}");
        assert!(warnings.is_empty(), "{warnings:?}");

        // An unknown name is rejected before the value is even looked at.
        let error = set_daemon_field(
            dir.path(),
            "no_such_key",
            SettingValue::Unset,
            &mut warnings,
        )
        .unwrap_err();
        assert!(
            error.message.contains("unknown daemon setting"),
            "{}",
            error.message
        );

        // The tui schema behaves the same: unset restores the "auto" state of truecolor.
        let files = crate::config::ConfigFiles {
            user_global: None,
            user_local: Some(dir.path().join("config.local.toml")),
        };
        std::fs::write(
            files.user_local.clone().unwrap(),
            "[tui]\ntruecolor = false\n",
        )
        .unwrap();
        set_tui_field(&files, "truecolor", SettingValue::Unset, &mut warnings).unwrap();
        let text = std::fs::read_to_string(files.user_local.clone().unwrap()).unwrap();
        assert!(!text.contains("truecolor"), "{text}");
    }

    #[test]
    fn the_readout_reports_every_registry_field() {
        let readout = daemon_readout(&DaemonSettings::default());
        assert_eq!(readout.len(), DAEMON_FIELDS.len());
        assert_eq!(
            readout.get("provider"),
            Some(&SettingValue::Str("claude".into()))
        );
        assert_eq!(
            readout.get("run_mode"),
            Some(&SettingValue::Str("sprint".into()))
        );
        assert_eq!(
            readout.get("agent_timeout_secs"),
            Some(&SettingValue::Uint(600))
        );
        assert_eq!(
            readout.get("discovery_cooldown_secs"),
            Some(&SettingValue::Uint(
                crate::config::DEFAULT_DISCOVERY_COOLDOWN_SECS
            ))
        );
        // A single-model override collapses the role models in the readout too.
        let materialized = {
            let raw = crate::config::daemon::DaemonLayer {
                model: Some("mistral-small".into()),
                ..Default::default()
            };
            crate::config::daemon::materialize(raw)
        };
        let readout = daemon_readout(&materialized);
        assert_eq!(
            readout.get("builder_model"),
            Some(&SettingValue::Str("mistral-small".into()))
        );
        assert_eq!(
            readout.get("model"),
            Some(&SettingValue::Str("mistral-small".into()))
        );
    }
}

#[cfg(test)]
mod research_key_tests {
    use super::*;

    #[test]
    fn the_renamed_research_key_persists_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        set_daemon_field(
            dir.path(),
            "skip_research_for_simple",
            SettingValue::Bool(false),
            &mut warnings,
        )
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let text = std::fs::read_to_string(dir.path().join("patok.config.toml")).unwrap();
        assert!(text.contains("skip_research_for_simple = false"), "{text}");
        // The readout and the diff see the renamed field.
        let settings = DaemonSettings {
            skip_research_for_simple: false,
            ..DaemonSettings::default()
        };
        assert_eq!(
            daemon_field_value(&settings, "skip_research_for_simple"),
            Some(SettingValue::Bool(false))
        );
        assert_eq!(
            daemon_changed_fields(&DaemonSettings::default(), &settings),
            ["skip_research_for_simple"]
        );
    }
}
