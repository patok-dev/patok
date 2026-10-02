//! The daemon schema: everything that parameterizes the
//! build loop, owned by the engine. The shell never reads this schema; it parses only the
//! `[daemon]` table of each config file.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use schemars::JsonSchema;

use super::{ConfigFiles, Fields, migrate_project_file, project_file, read_layer_table};

/// The providers the engine can run today.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProviderKind {
    #[default]
    Claude,
    Codex,
    Mistral,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Mistral => "mistral",
        }
    }
}

impl fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A provider name as written in config files:
/// trimmed, case-insensitive, aliases accepted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
#[schemars(rename_all = "lowercase")]
pub enum ProviderName {
    #[default]
    Claude,
    Codex,
    OpenCode,
    GhCopilot,
    Mistral,
}

impl ProviderName {
    pub const ACCEPTED_SPELLINGS: &str = "claude, codex, opencode, ghcopilot (aliases gh-copilot, github-copilot, copilot), mistral (alias vibe)";

    /// Parses a config spelling; `None` for anything unrecognized.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::OpenCode),
            "ghcopilot" | "gh-copilot" | "github-copilot" | "copilot" => Some(Self::GhCopilot),
            "mistral" | "vibe" => Some(Self::Mistral),
            _ => None,
        }
    }

    /// The primary config spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::GhCopilot => "ghcopilot",
            Self::Mistral => "mistral",
        }
    }

    /// The engine-implemented kind, or `None` for a provider this build does not ship yet.
    pub fn kind(self) -> Option<ProviderKind> {
        match self {
            Self::Claude => Some(ProviderKind::Claude),
            Self::Codex => Some(ProviderKind::Codex),
            Self::Mistral => Some(ProviderKind::Mistral),
            Self::OpenCode | Self::GhCopilot => None,
        }
    }
}

impl fmt::Display for ProviderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The run mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
#[schemars(rename_all = "lowercase")]
pub enum RunMode {
    #[default]
    Sprint,
    Continuous,
}

impl RunMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "sprint" => Some(Self::Sprint),
            "continuous" => Some(Self::Continuous),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sprint => "sprint",
            Self::Continuous => "continuous",
        }
    }
}

impl fmt::Display for RunMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The plan self-review accept policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, JsonSchema)]
#[schemars(rename_all = "kebab-case")]
pub enum AcceptPolicy {
    #[default]
    NoHigh,
    NoHighMedium,
    NoFindings,
}

impl AcceptPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "no-high" => Some(Self::NoHigh),
            "no-high-medium" => Some(Self::NoHighMedium),
            "no-findings" => Some(Self::NoFindings),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoHigh => "no-high",
            Self::NoHighMedium => "no-high-medium",
            Self::NoFindings => "no-findings",
        }
    }
}

impl fmt::Display for AcceptPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One pipeline stage: id, label, enabled flag and an optional
/// prompt override. A stage absent from the list is treated as disabled by the consumer;
/// order is display-only.
#[derive(Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct Stage {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub prompt: Option<String>,
}

/// A pipeline stage as written in one layer, before merging: each field may be absent,
/// and a higher layer's entry overrides only the fields it sets.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RawStage {
    pub id: String,
    pub label: Option<String>,
    pub enabled: Option<bool>,
    pub prompt: Option<String>,
}

/// The merged, normalized daemon settings.
///
/// Fields beyond `provider`, `model`, `plan_enabled` and the discovery cooldowns are
/// parsed, merged and normalized here but not yet consumed by engine behaviour; the
/// pipeline, routing and learnings tasks pick them up.
#[derive(Clone, Debug, PartialEq, JsonSchema)]
pub struct DaemonSettings {
    // Routing (section 4).
    pub provider: ProviderName,
    /// The single-model override; empty means none (normalization applies it per role).
    pub model: Option<String>,
    pub research_model: String,
    pub planner_model: String,
    pub builder_model: String,
    pub reviewer_model: String,
    pub discovery_model: String,
    pub learning_extraction_model: String,
    pub research_provider: ProviderName,
    pub planner_provider: ProviderName,
    pub builder_provider: ProviderName,
    pub reviewer_provider: ProviderName,
    pub discovery_provider: ProviderName,
    pub learning_extraction_provider: ProviderName,
    pub local_model: Option<String>,
    pub model_catalog_refresh_secs: u64,
    pub catalog_url_overrides: BTreeMap<String, String>,
    pub stage_overrides: Vec<String>,
    // Pipeline flow (section 5).
    pub run_mode: RunMode,
    pub stages: Vec<Stage>,
    pub skip_planner_for_simple: bool,
    /// Whether a Simple-classified task skips the research stage (T68.1).
    pub skip_research_for_simple: bool,
    pub skip_review_for_simple: bool,
    pub review_confidence_threshold: u64,
    pub batch_review: bool,
    pub planner_lookahead: bool,
    pub plan_revision_cycles: u64,
    pub plan_revision_accept_policy: AcceptPolicy,
    pub review_in_loop: bool,
    pub review_multipass_threshold: u64,
    pub confidence_threshold: f64,
    pub semgrep_enabled: bool,
    pub semgrep_rulesets: Vec<String>,
    pub build_command: Option<String>,
    pub planning_iterations: u64,
    pub require_human_approval: bool,
    pub auto_push_remote: Option<String>,
    pub on_task_complete_hook: Option<String>,
    pub discovery_cooldown_secs: u64,
    pub discovery_cooldown_cap_secs: u64,
    /// Whether a plan session runs before each builder session (T10.1).
    pub plan_enabled: bool,
    // Timing (section 6).
    pub agent_timeout_secs: u64,
    pub pause_between_tasks_secs: u64,
    pub pause_between_agents_secs: u64,
    pub pause_between_cycles_secs: u64,
    pub adaptive_pauses: bool,
    pub engine_idle_timeout_secs: u64,
    // Isolation and security (section 7).
    pub phase_isolation: bool,
    pub enforce_phase_rbac: bool,
    // Learnings, history and embeddings (section 8).
    pub history_dir: Option<String>,
    pub max_learning_injection: u64,
    pub min_learning_injection: u64,
    pub history_search_results: u64,
    pub history_retention_tasks: u64,
    pub observatory_retention_days: u64,
    pub semantic_match_enabled: bool,
    pub embedding_model: String,
    pub embedding_port: u16,
    pub embedding_timeout_ms: u64,
    pub plugins: Vec<String>,
    pub show_retrieval_panel: bool,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        materialize(DaemonLayer::default())
    }
}

/// One layer's `[daemon]` table; unset keys fall through to the layer below.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DaemonLayer {
    pub provider: Option<ProviderName>,
    pub model: Option<String>,
    pub research_model: Option<String>,
    pub planner_model: Option<String>,
    pub builder_model: Option<String>,
    pub reviewer_model: Option<String>,
    pub discovery_model: Option<String>,
    pub learning_extraction_model: Option<String>,
    pub research_provider: Option<ProviderName>,
    pub planner_provider: Option<ProviderName>,
    pub builder_provider: Option<ProviderName>,
    pub reviewer_provider: Option<ProviderName>,
    pub discovery_provider: Option<ProviderName>,
    pub learning_extraction_provider: Option<ProviderName>,
    pub local_model: Option<String>,
    pub model_catalog_refresh_secs: Option<u64>,
    pub catalog_url_overrides: Option<BTreeMap<String, String>>,
    pub stage_overrides: Option<Vec<String>>,
    pub run_mode: Option<RunMode>,
    pub stages: Option<Vec<RawStage>>,
    pub skip_planner_for_simple: Option<bool>,
    pub skip_research_for_simple: Option<bool>,
    pub skip_review_for_simple: Option<bool>,
    pub review_confidence_threshold: Option<u64>,
    pub batch_review: Option<bool>,
    pub planner_lookahead: Option<bool>,
    pub plan_revision_cycles: Option<u64>,
    pub plan_revision_accept_policy: Option<AcceptPolicy>,
    pub review_in_loop: Option<bool>,
    pub review_multipass_threshold: Option<u64>,
    pub confidence_threshold: Option<f64>,
    pub semgrep_enabled: Option<bool>,
    pub semgrep_rulesets: Option<Vec<String>>,
    pub build_command: Option<String>,
    pub planning_iterations: Option<u64>,
    pub require_human_approval: Option<bool>,
    pub auto_push_remote: Option<String>,
    pub on_task_complete_hook: Option<String>,
    pub discovery_cooldown_secs: Option<u64>,
    pub discovery_cooldown_cap_secs: Option<u64>,
    pub plan_enabled: Option<bool>,
    pub agent_timeout_secs: Option<u64>,
    pub pause_between_tasks_secs: Option<u64>,
    pub pause_between_agents_secs: Option<u64>,
    pub pause_between_cycles_secs: Option<u64>,
    pub adaptive_pauses: Option<bool>,
    pub engine_idle_timeout_secs: Option<u64>,
    pub phase_isolation: Option<bool>,
    pub enforce_phase_rbac: Option<bool>,
    pub history_dir: Option<String>,
    pub max_learning_injection: Option<u64>,
    pub min_learning_injection: Option<u64>,
    pub history_search_results: Option<u64>,
    pub history_retention_tasks: Option<u64>,
    pub observatory_retention_days: Option<u64>,
    pub semantic_match_enabled: Option<bool>,
    pub embedding_model: Option<String>,
    pub embedding_port: Option<u16>,
    pub embedding_timeout_ms: Option<u64>,
    pub plugins: Option<Vec<String>>,
    pub show_retrieval_panel: Option<bool>,
}

/// The daemon-schema environment overrides: the agent
/// timeout, under two variable names (the first valid one wins).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DaemonEnv {
    /// [`PATOK_AGENT_TIMEOUT_SECS`] and its alias [`PATOK_AGENT_TIMEOUT`].
    pub agent_timeout_secs: [Option<String>; 2],
}

impl DaemonEnv {
    pub const AGENT_TIMEOUT_VARS: [&'static str; 2] =
        ["PATOK_AGENT_TIMEOUT_SECS", "PATOK_AGENT_TIMEOUT"];

    pub fn from_process_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        Self {
            agent_timeout_secs: [
                var(Self::AGENT_TIMEOUT_VARS[0]),
                var(Self::AGENT_TIMEOUT_VARS[1]),
            ],
        }
    }
}

/// The result of loading the daemon schema.
#[derive(Clone, Debug, Default)]
pub struct DaemonConfig {
    pub settings: DaemonSettings,
    /// Skip-and-fallback warnings; the caller logs them.
    pub warnings: Vec<String>,
    /// Hard errors that must refuse runs: unrecognized provider values.
    pub errors: Vec<String>,
}

/// Loads and merges the daemon schema across the four file layers and the environment.
/// The project layer lives in `data_dir` (the project's slot of the projects dir); a
/// legacy project-root config is migrated there first.
pub fn load_daemon(
    files: &ConfigFiles,
    project_dir: &Path,
    data_dir: &Path,
    env: &DaemonEnv,
) -> DaemonConfig {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    let mut merged = DaemonLayer::default();
    migrate_project_file(project_dir, data_dir, &mut warnings);
    for path in [
        files.user_global.as_deref(),
        files.user_local.as_deref(),
        Some(project_file(data_dir).as_path()),
    ]
    .into_iter()
    .flatten()
    {
        let Some(table) = read_layer_table(path, "daemon", "tui", &mut warnings) else {
            continue;
        };
        let layer = parse_layer(&table, path, &mut warnings, &mut errors);
        merged = super::merge::daemon(merged, layer);
    }
    let mut settings = materialize(merged);
    apply_env(&mut settings, env, &mut warnings);
    DaemonConfig {
        settings,
        warnings,
        errors,
    }
}

/// Parses one file's `[daemon]` table into a raw layer. Wrong types fall back to the
/// field's default with a warning; unknown keys warn; unrecognized provider values push
/// a hard error onto `errors` (the field still falls back to its default).
pub(crate) fn parse_layer(
    table: &toml::Table,
    path: &Path,
    warnings: &mut Vec<String>,
    errors: &mut Vec<String>,
) -> DaemonLayer {
    let mut layer = DaemonLayer::default();
    let mut f = Fields {
        path,
        schema: "daemon",
        warnings,
    };
    for (key, value) in table {
        match key.as_str() {
            // Routing (section 4).
            "provider" => layer.provider = f.provider(key, value, errors),
            "model" => layer.model = f.string(key, value),
            "research_model" => layer.research_model = f.string(key, value),
            "planner_model" => layer.planner_model = f.string(key, value),
            "builder_model" => layer.builder_model = f.string(key, value),
            "reviewer_model" => layer.reviewer_model = f.string(key, value),
            "discovery_model" => layer.discovery_model = f.string(key, value),
            "learning_extraction_model" => layer.learning_extraction_model = f.string(key, value),
            "research_provider" => layer.research_provider = f.provider(key, value, errors),
            "planner_provider" => layer.planner_provider = f.provider(key, value, errors),
            "builder_provider" => layer.builder_provider = f.provider(key, value, errors),
            "reviewer_provider" => layer.reviewer_provider = f.provider(key, value, errors),
            "discovery_provider" => layer.discovery_provider = f.provider(key, value, errors),
            "learning_extraction_provider" => {
                layer.learning_extraction_provider = f.provider(key, value, errors);
            }
            "local_model" => layer.local_model = f.string(key, value),
            "model_catalog_refresh_secs" => {
                layer.model_catalog_refresh_secs =
                    f.uint(key, value, "a whole number of seconds", false);
            }
            "catalog_url_overrides" => layer.catalog_url_overrides = f.string_map(key, value),
            "stage_overrides" => layer.stage_overrides = f.strings(key, value),
            // Pipeline flow (section 5).
            "run_mode" => {
                layer.run_mode = f.enumerated(key, value, "`sprint`, `continuous`", RunMode::parse);
            }
            "stages" => layer.stages = f.stages(value),
            "skip_planner_for_simple" => layer.skip_planner_for_simple = f.boolean(key, value),
            "skip_research_for_simple" => {
                layer.skip_research_for_simple = f.boolean(key, value);
            }
            "skip_review_for_simple" => layer.skip_review_for_simple = f.boolean(key, value),
            "review_confidence_threshold" => {
                layer.review_confidence_threshold = f.uint(key, value, "a whole number", false);
            }
            "batch_review" => layer.batch_review = f.boolean(key, value),
            "planner_lookahead" => layer.planner_lookahead = f.boolean(key, value),
            "plan_revision_cycles" => {
                layer.plan_revision_cycles = f.uint(key, value, "a whole number", false);
            }
            "plan_revision_accept_policy" => {
                layer.plan_revision_accept_policy = f.enumerated(
                    key,
                    value,
                    "`no-high`, `no-high-medium`, `no-findings`",
                    AcceptPolicy::parse,
                );
            }
            "review_in_loop" => layer.review_in_loop = f.boolean(key, value),
            "review_multipass_threshold" => {
                layer.review_multipass_threshold = f.uint(key, value, "a whole number", false);
            }
            "confidence_threshold" => {
                layer.confidence_threshold = f.float(key, value, Some((0.0, 1.0)));
            }
            "semgrep_enabled" => layer.semgrep_enabled = f.boolean(key, value),
            "semgrep_rulesets" => layer.semgrep_rulesets = f.strings(key, value),
            "build_command" => layer.build_command = f.string(key, value),
            "planning_iterations" => {
                layer.planning_iterations = f.uint(key, value, "a whole number", false);
            }
            "require_human_approval" => layer.require_human_approval = f.boolean(key, value),
            "auto_push_remote" => layer.auto_push_remote = f.string(key, value),
            "on_task_complete_hook" => layer.on_task_complete_hook = f.string(key, value),
            "discovery_cooldown_secs" => {
                layer.discovery_cooldown_secs = f.uint(
                    key,
                    value,
                    "a whole number of seconds greater than zero",
                    true,
                );
            }
            "discovery_cooldown_cap_secs" => {
                layer.discovery_cooldown_cap_secs = f.uint(
                    key,
                    value,
                    "a whole number of seconds greater than zero",
                    true,
                );
            }
            "plan_enabled" => layer.plan_enabled = f.boolean(key, value),
            // Timing (section 6).
            "agent_timeout_secs" => {
                layer.agent_timeout_secs = f.uint(
                    key,
                    value,
                    "a whole number of seconds greater than zero",
                    true,
                );
            }
            "pause_between_tasks_secs" => {
                layer.pause_between_tasks_secs =
                    f.uint(key, value, "a whole number of seconds", false);
            }
            "pause_between_agents_secs" => {
                layer.pause_between_agents_secs =
                    f.uint(key, value, "a whole number of seconds", false);
            }
            "pause_between_cycles_secs" => {
                layer.pause_between_cycles_secs =
                    f.uint(key, value, "a whole number of seconds", false);
            }
            "adaptive_pauses" => layer.adaptive_pauses = f.boolean(key, value),
            "engine_idle_timeout_secs" => {
                layer.engine_idle_timeout_secs =
                    f.uint(key, value, "a whole number of seconds", false);
            }
            // Isolation and security (section 7).
            "phase_isolation" => layer.phase_isolation = f.boolean(key, value),
            "enforce_phase_rbac" => layer.enforce_phase_rbac = f.boolean(key, value),
            // Learnings, history and embeddings (section 8).
            "history_dir" => layer.history_dir = f.string(key, value),
            "max_learning_injection" => {
                layer.max_learning_injection = f.uint(key, value, "a whole number", false);
            }
            "min_learning_injection" => {
                layer.min_learning_injection = f.uint(key, value, "a whole number", false);
            }
            "history_search_results" => {
                layer.history_search_results = f.uint(key, value, "a whole number", false);
            }
            "history_retention_tasks" => {
                layer.history_retention_tasks = f.uint(key, value, "a whole number", false);
            }
            "observatory_retention_days" => {
                layer.observatory_retention_days = f.uint(key, value, "a whole number", false);
            }
            "semantic_match_enabled" => layer.semantic_match_enabled = f.boolean(key, value),
            "embedding_model" => layer.embedding_model = f.string(key, value),
            "embedding_port" => layer.embedding_port = f.port(key, value),
            "embedding_timeout_ms" => {
                layer.embedding_timeout_ms =
                    f.uint(key, value, "a whole number of milliseconds", false);
            }
            "plugins" => layer.plugins = f.strings(key, value),
            "show_retrieval_panel" => layer.show_retrieval_panel = f.boolean(key, value),
            other => f.unknown(other),
        }
    }
    layer
}

impl Fields<'_> {
    /// The pipeline stage list (`[[daemon.stages]]`): merged by id across layers. An entry
    /// without a usable id is skipped with a warning; each of its fields with a wrong
    /// type falls back to that field's default.
    fn stages(&mut self, value: &toml::Value) -> Option<Vec<RawStage>> {
        let Some(entries) = value.as_array() else {
            self.warnings.push(format!(
                "{}: {}.stages must be an array of stage tables, got {value}; using the default",
                self.path.display(),
                self.schema
            ));
            return None;
        };
        let mut stages = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let Some(table) = entry.as_table() else {
                self.warnings.push(format!(
                    "{}: {}.stages entry {index} must be a table, got {entry}; skipping it",
                    self.path.display(),
                    self.schema
                ));
                continue;
            };
            let Some(id) = table
                .get("id")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                self.warnings.push(format!(
                    "{}: {}.stages entry {index} must have a non-empty string `id`; skipping it",
                    self.path.display(),
                    self.schema
                ));
                continue;
            };
            let mut stage = RawStage {
                id: id.to_string(),
                ..RawStage::default()
            };
            for (field, value) in table {
                match field.as_str() {
                    "id" => {}
                    "label" => {
                        stage.label = self.string(&format!("stages entry `{id}` label"), value)
                    }
                    "enabled" => {
                        stage.enabled = self.boolean(&format!("stages entry `{id}` enabled"), value)
                    }
                    "prompt" => {
                        stage.prompt = self.string(&format!("stages entry `{id}` prompt"), value)
                    }
                    other => self.unknown(&format!("stages entry `{id}` key `{other}`")),
                }
            }
            stages.push(stage);
        }
        Some(stages)
    }
}

/// The four default pipeline stages.
fn default_stages() -> Vec<Stage> {
    [
        ("research", "RESEARCH"),
        ("plan", "PLAN"),
        ("implement", "BUILD"),
        ("review", "REVIEW"),
    ]
    .into_iter()
    .map(|(id, label)| Stage {
        id: id.into(),
        label: label.into(),
        enabled: true,
        prompt: None,
    })
    .collect()
}

/// Turns the merged raw layer into the final settings: hardcoded defaults for unset
/// fields, then the single-model override
///.
pub(crate) fn materialize(layer: DaemonLayer) -> DaemonSettings {
    let reviewer_provider = layer.reviewer_provider;
    let mut settings = DaemonSettings {
        provider: layer.provider.unwrap_or_default(),
        model: layer.model,
        research_model: layer.research_model.unwrap_or_else(|| "sonnet".into()),
        planner_model: layer.planner_model.unwrap_or_else(|| "opus".into()),
        builder_model: layer.builder_model.unwrap_or_else(|| "opus".into()),
        reviewer_model: layer.reviewer_model.unwrap_or_else(|| "sonnet".into()),
        discovery_model: layer.discovery_model.unwrap_or_else(|| "opus".into()),
        learning_extraction_model: layer
            .learning_extraction_model
            .unwrap_or_else(|| "sonnet".into()),
        research_provider: layer.research_provider.unwrap_or_default(),
        planner_provider: layer.planner_provider.unwrap_or_default(),
        builder_provider: layer.builder_provider.unwrap_or_default(),
        reviewer_provider: reviewer_provider.unwrap_or_default(),
        discovery_provider: layer.discovery_provider.unwrap_or_default(),
        learning_extraction_provider: layer.learning_extraction_provider.unwrap_or_default(),
        local_model: layer.local_model,
        model_catalog_refresh_secs: layer.model_catalog_refresh_secs.unwrap_or(86400),
        catalog_url_overrides: layer.catalog_url_overrides.unwrap_or_default(),
        stage_overrides: layer.stage_overrides.unwrap_or_default(),
        run_mode: layer.run_mode.unwrap_or_default(),
        stages: layer
            .stages
            .map(|stages| {
                stages
                    .into_iter()
                    .map(|stage| Stage {
                        label: stage.label.unwrap_or_else(|| stage.id.clone()),
                        enabled: stage.enabled.unwrap_or(true),
                        prompt: stage.prompt,
                        id: stage.id,
                    })
                    .collect()
            })
            .unwrap_or_else(default_stages),
        skip_planner_for_simple: layer.skip_planner_for_simple.unwrap_or(true),
        skip_research_for_simple: layer.skip_research_for_simple.unwrap_or(true),
        skip_review_for_simple: layer.skip_review_for_simple.unwrap_or(true),
        review_confidence_threshold: layer.review_confidence_threshold.unwrap_or(5),
        batch_review: layer.batch_review.unwrap_or(true),
        planner_lookahead: layer.planner_lookahead.unwrap_or(true),
        plan_revision_cycles: layer.plan_revision_cycles.unwrap_or(0),
        plan_revision_accept_policy: layer.plan_revision_accept_policy.unwrap_or_default(),
        review_in_loop: layer.review_in_loop.unwrap_or(true),
        review_multipass_threshold: layer.review_multipass_threshold.unwrap_or(8),
        confidence_threshold: layer.confidence_threshold.unwrap_or(0.5),
        semgrep_enabled: layer.semgrep_enabled.unwrap_or(false),
        semgrep_rulesets: layer.semgrep_rulesets.unwrap_or_default(),
        build_command: layer.build_command,
        planning_iterations: layer.planning_iterations.unwrap_or(0),
        require_human_approval: layer.require_human_approval.unwrap_or(false),
        auto_push_remote: layer.auto_push_remote,
        on_task_complete_hook: layer.on_task_complete_hook,
        discovery_cooldown_secs: layer
            .discovery_cooldown_secs
            .unwrap_or(super::DEFAULT_DISCOVERY_COOLDOWN_SECS),
        discovery_cooldown_cap_secs: layer
            .discovery_cooldown_cap_secs
            .unwrap_or(super::DEFAULT_DISCOVERY_COOLDOWN_CAP_SECS),
        plan_enabled: layer.plan_enabled.unwrap_or(true),
        agent_timeout_secs: layer.agent_timeout_secs.unwrap_or(600),
        pause_between_tasks_secs: layer.pause_between_tasks_secs.unwrap_or(10),
        pause_between_agents_secs: layer.pause_between_agents_secs.unwrap_or(3),
        pause_between_cycles_secs: layer.pause_between_cycles_secs.unwrap_or(30),
        adaptive_pauses: layer.adaptive_pauses.unwrap_or(true),
        engine_idle_timeout_secs: layer.engine_idle_timeout_secs.unwrap_or(1800),
        phase_isolation: layer.phase_isolation.unwrap_or(true),
        enforce_phase_rbac: layer.enforce_phase_rbac.unwrap_or(true),
        history_dir: layer.history_dir,
        max_learning_injection: layer.max_learning_injection.unwrap_or(10),
        min_learning_injection: layer.min_learning_injection.unwrap_or(2),
        history_search_results: layer.history_search_results.unwrap_or(5),
        history_retention_tasks: layer.history_retention_tasks.unwrap_or(50),
        observatory_retention_days: layer.observatory_retention_days.unwrap_or(30),
        semantic_match_enabled: layer.semantic_match_enabled.unwrap_or(true),
        embedding_model: layer
            .embedding_model
            .unwrap_or_else(|| "nomic-embed-text".into()),
        embedding_port: layer.embedding_port.unwrap_or(11434),
        embedding_timeout_ms: layer.embedding_timeout_ms.unwrap_or(2000),
        plugins: layer.plugins.unwrap_or_default(),
        show_retrieval_panel: layer.show_retrieval_panel.unwrap_or(false),
    };
    // Normalization: a non-empty single-model override replaces the models of
    // research, planner, builder, reviewer, discovery and learning extraction.
    if let Some(model) = settings.model.clone() {
        settings.research_model = model.clone();
        settings.planner_model = model.clone();
        settings.builder_model = model.clone();
        settings.reviewer_model = model.clone();
        settings.discovery_model = model.clone();
        settings.learning_extraction_model = model;
    }
    settings
}

/// The help text for one settings-overlay key of the daemon schema (T85.1):
/// the description of a setting row, or of a whole overlay section when the
/// key is a section's group id (`provider_and_model`, `pipeline`,
/// `timeouts_and_pauses`, `git`). Hardcoded here, on the side that owns the settings it describes;
/// the shell routes to it by the row's declared schema. `None` for a key that
/// is not one of the overlay's daemon rows or groups.
pub fn daemon_help(key: &str) -> Option<&'static str> {
    Some(match key {
        "provider_and_model" => {
            "Which CLI provider and model the agent roles run on. A change applies at the next task or agent start; whatever is already running keeps the value it started with."
        }
        "pipeline" => {
            "How tasks flow through the pipeline: which stages may be skipped for simple tasks, how the reviewer is scheduled, and how its findings are triaged."
        }
        "timeouts_and_pauses" => {
            "Timing: how long an agent may idle, the pauses between tasks, agents and cycles, and how long an engine with no shell attached waits before shutting down."
        }
        "git" => "How finished work reaches git.",
        "engine_version" => "The engine's version, as reported by the daemon. Read-only.",
        "shell_version" => "The shell's own version. Read-only.",
        "provider" => {
            "The provider every agent role runs on: claude, codex, opencode, ghcopilot or mistral. Any unrecognized value behaves as claude."
        }
        "model" => {
            "Single-model override: when set, it replaces the model of every agent role (research, planner, builder, reviewer, discovery, learning extraction). Meant for subscriptions locked to one model; empty means each role keeps its own model."
        }
        "research_provider" => {
            "The provider the research role runs on. Applies at the next research session start."
        }
        "research_model" => {
            "The model the research role runs on; empty uses the provider's default. Ignored while the single-model override is set."
        }
        "planner_provider" => {
            "The provider the planner role runs on. Applies at the next planner session start."
        }
        "planner_model" => {
            "The model the planner role runs on; empty uses the provider's default. Ignored while the single-model override is set."
        }
        "builder_provider" => {
            "The provider the builder role runs on. Applies at the next builder session start."
        }
        "builder_model" => {
            "The model the builder role runs on; empty uses the provider's default. Ignored while the single-model override is set."
        }
        "reviewer_provider" => {
            "The provider the reviewer role runs on: claude (default), codex, opencode, ghcopilot or mistral (vibe is accepted as an alias for mistral). Applies at the next reviewer session start."
        }
        "reviewer_model" => {
            "The model the reviewer role runs on; empty uses the provider's default. Ignored while the single-model override is set."
        }
        "discovery_provider" => {
            "The provider the discovery role runs on. Applies at the next discovery session start."
        }
        "discovery_model" => {
            "The model the discovery role runs on; empty uses the provider's default. Ignored while the single-model override is set."
        }
        "run_mode" => {
            "sprint (default) or continuous. The running loop keeps the mode it started with; a change applies at the next run."
        }
        "plan_enabled" => "Whether a plan session runs before each builder session.",
        "skip_planner_for_simple" => "Simple tasks go straight to the builder (default on).",
        "skip_research_for_simple" => "Simple tasks skip the research stage (default on).",
        "skip_review_for_simple" => {
            "Simple tasks skip the review stage, only when the builder's verification exited cleanly (default on)."
        }
        "review_confidence_threshold" => {
            "Consecutive review passes needed before learned confidence may skip the review for matching task shapes (simple and medium tasks only); 0 disables. Default 5."
        }
        "review_multipass_threshold" => {
            "Above this many changed files the reviewer splits into per-file passes plus one integration pass; 0 disables. Default 8."
        }
        "batch_review" => {
            "Skip the review for all but the last task in each contiguous run of same-numbered task IDs; that task's review covers every commit in the run (default on)."
        }
        "planner_lookahead" => {
            "Plan task N+1 while task N builds; the plan is reused when that task comes up (default on)."
        }
        "review_in_loop" => {
            "Run the reviewer stage; when off, a task counts as validated by the builder's verification alone (default on)."
        }
        "confidence_threshold" => {
            "Findings below this confidence are logged for manual review instead of being auto-fixed. Range 0.0-1.0, default 0.5."
        }
        "agent_timeout_secs" => {
            "How long one agent invocation may idle, in seconds, before it is stopped; the hard timeout is four times this. Default 600."
        }
        "pause_between_tasks_secs" => "Pause between two tasks, in seconds. Default 10.",
        "pause_between_agents_secs" => {
            "Pause between two agent invocations, in seconds. Default 3."
        }
        "pause_between_cycles_secs" => "Pause between build cycles, in seconds. Default 30.",
        "engine_idle_timeout_secs" => {
            "When no shell is attached and the engine has been idle this long, it shuts itself down. Default 1800 (30 minutes)."
        }
        "adaptive_pauses" => {
            "Shorten the pauses to about 500 ms when the last agent was not rate limited (default on)."
        }
        "discovery_cooldown_secs" => {
            "Minimum time between discovery rounds, in seconds. Default 300 (5 minutes)."
        }
        "discovery_cooldown_cap_secs" => {
            "The discovery cooldown doubles after each round, up to this cap in seconds. Default 1800 (30 minutes)."
        }
        "auto_push_remote" => {
            "Push to this remote after each successful commit; empty means local commits only."
        }
        _ => return None,
    })
}

/// Applies the daemon-schema environment overrides: the agent
/// timeout, under two variable names (the first valid one winning); empty or invalid
/// values are ignored with a warning.
pub(crate) fn apply_env(
    settings: &mut DaemonSettings,
    env: &DaemonEnv,
    warnings: &mut Vec<String>,
) {
    let warn = |warnings: &mut Vec<String>, name: &str, value: &str| {
        warnings.push(format!("ignoring invalid {name}={value:?}"));
    };
    for (name, value) in DaemonEnv::AGENT_TIMEOUT_VARS
        .iter()
        .zip(&env.agent_timeout_secs)
    {
        let Some(value) = value else { continue };
        match value.trim().parse::<u64>() {
            Ok(secs) if secs > 0 => {
                settings.agent_timeout_secs = secs;
                break;
            }
            _ => warn(warnings, name, value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon schema loaded from a project-local config file holding `text`,
    /// written into the project's slot of the projects dir.
    fn load_project(text: &str) -> DaemonConfig {
        let project = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(data.path().join("patok.config.toml"), text).unwrap();
        load_daemon(
            &ConfigFiles {
                user_global: None,
                user_local: None,
            },
            project.path(),
            data.path(),
            &DaemonEnv::default(),
        )
    }

    #[test]
    fn skip_research_for_simple_parses_and_defaults_true() {
        let loaded = load_project("[daemon]\nskip_research_for_simple = false\n");
        assert!(!loaded.settings.skip_research_for_simple);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(DaemonSettings::default().skip_research_for_simple);
    }

    #[test]
    fn skip_review_for_simple_parses_and_defaults_true() {
        let loaded = load_project("[daemon]\nskip_review_for_simple = false\n");
        assert!(!loaded.settings.skip_review_for_simple);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(DaemonSettings::default().skip_review_for_simple);
    }

    #[test]
    fn an_unknown_daemon_key_warns_without_refusing_runs() {
        // Keys removed from the schema (or simply mistyped) fall through to the
        // generic unknown-key arm: the config loads with defaults and a warning.
        let loaded = load_project("[daemon]\nreticulate_splines = true\n");
        assert_eq!(
            loaded.settings.reviewer_provider,
            DaemonSettings::default().reviewer_provider
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.contains("ignoring unknown key daemon.")),
            "{:?}",
            loaded.warnings
        );
    }

    #[test]
    fn provider_codex_resolves_to_the_codex_kind() {
        let loaded = load_project("[daemon]\nprovider = \"codex\"\n");
        assert_eq!(loaded.settings.provider, ProviderName::Codex);
        assert_eq!(loaded.settings.provider.kind(), Some(ProviderKind::Codex));
        assert_eq!(ProviderKind::Codex.as_str(), "codex");
    }

    /// The help registry (T85.1) answers every overlay key it owns with
    /// non-empty text, and nothing else.
    #[test]
    fn daemon_help_covers_its_keys_and_rejects_unknown_ones() {
        for key in [
            "provider",
            "agent_timeout_secs",
            "auto_push_remote",
            "pipeline",
        ] {
            let help = daemon_help(key).unwrap_or_else(|| panic!("`{key}` has no help text"));
            assert!(!help.is_empty());
        }
        // A field the overlay deliberately excludes stays out, as do groups of
        // the other schema.
        assert_eq!(daemon_help("plan_revision_cycles"), None);
        assert_eq!(daemon_help("theme"), None);
    }
}
