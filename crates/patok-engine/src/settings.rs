//! The wire `SettingsChange` mapped onto the daemon registry's key names and values.

use patok_core::config::SettingValue;
use patok_proto::settings_change::Change;
use patok_proto::{AcceptPolicy, Provider, Role, RunMode};

/// The enum value of a raw wire number; the zero value when the number is outside the
/// enum (only possible from a mismatched client, since this is an internal protocol).
fn of<T>(raw: i32) -> T
where
    T: Default + TryFrom<i32>,
{
    T::try_from(raw).unwrap_or_default()
}

/// The on-disk `[daemon]` key and the submitted value of one settings change. Every case
/// maps to its registry name; names absent from the registry (the orchestrator seats)
/// flow on uniformly so the persist layer rejects them with the same "unknown daemon
/// setting" error as any other unrecognized field.
pub fn from_proto(change: &Change) -> (String, SettingValue) {
    let str = |value: &str| SettingValue::Str(value.to_string());
    match change {
        // -- Section 4, model and provider routing --
        Change::RoleModel(role) => (
            format!("{}_model", role_name(of(role.role))),
            str(&role.model),
        ),
        Change::RoleProvider(role) => (
            format!("{}_provider", role_name(of(role.role))),
            str(provider_name(of(role.provider))),
        ),
        Change::ModelOverride(model) => ("model".into(), str(model)),
        Change::LocalModel(model) => ("local_model".into(), str(model)),
        Change::ModelCatalogRefreshSecs(secs) => (
            "model_catalog_refresh_secs".into(),
            SettingValue::Uint(u64::from(*secs)),
        ),
        Change::Provider(provider) => ("provider".into(), str(provider_name(of(*provider)))),
        // -- Section 5, run modes and pipeline flow --
        Change::RunMode(mode) => ("run_mode".into(), str(run_mode_name(of(*mode)))),
        Change::SkipPlannerForSimple(v) => {
            ("skip_planner_for_simple".into(), SettingValue::Bool(*v))
        }
        Change::SkipResearchForSimple(v) => {
            ("skip_research_for_simple".into(), SettingValue::Bool(*v))
        }
        Change::SkipReviewForSimple(v) => ("skip_review_for_simple".into(), SettingValue::Bool(*v)),
        Change::ReviewConfidenceThreshold(v) => (
            "review_confidence_threshold".into(),
            SettingValue::Uint(u64::from(*v)),
        ),
        Change::BatchReview(v) => ("batch_review".into(), SettingValue::Bool(*v)),
        Change::PlannerLookahead(v) => ("planner_lookahead".into(), SettingValue::Bool(*v)),
        Change::PlanRevisionCycles(v) => (
            "plan_revision_cycles".into(),
            SettingValue::Uint(u64::from(*v)),
        ),
        Change::PlanRevisionAcceptPolicy(policy) => (
            "plan_revision_accept_policy".into(),
            str(accept_policy_name(of(*policy))),
        ),
        Change::OrchestratorMaxIterations(v) => (
            "orchestrator_max_iterations".into(),
            SettingValue::Uint(u64::from(*v)),
        ),
        Change::OrchestratorAcceptPolicy(policy) => (
            "orchestrator_accept_policy".into(),
            str(accept_policy_name(of(*policy))),
        ),
        Change::ReviewInLoop(v) => ("review_in_loop".into(), SettingValue::Bool(*v)),
        Change::ReviewMultipassThreshold(v) => (
            "review_multipass_threshold".into(),
            SettingValue::Uint(u64::from(*v)),
        ),
        Change::ConfidenceThreshold(v) => ("confidence_threshold".into(), SettingValue::Float(*v)),
        Change::SemgrepEnabled(v) => ("semgrep_enabled".into(), SettingValue::Bool(*v)),
        Change::BuildCommand(v) => ("build_command".into(), str(v)),
        Change::PlanningIterations(v) => (
            "planning_iterations".into(),
            SettingValue::Uint(u64::from(*v)),
        ),
        Change::RequireHumanApproval(v) => {
            ("require_human_approval".into(), SettingValue::Bool(*v))
        }
        Change::AutoPushRemote(v) => ("auto_push_remote".into(), str(v)),
        Change::OnTaskCompleteHook(v) => ("on_task_complete_hook".into(), str(v)),
        Change::DiscoveryCooldownSecs(secs) => (
            "discovery_cooldown_secs".into(),
            SettingValue::Uint(u64::from(*secs)),
        ),
        Change::DiscoveryCooldownCapSecs(secs) => (
            "discovery_cooldown_cap_secs".into(),
            SettingValue::Uint(u64::from(*secs)),
        ),
        Change::PlanEnabled(v) => ("plan_enabled".into(), SettingValue::Bool(*v)),
        // -- Section 6, timing --
        Change::AgentTimeoutSecs(secs) => (
            "agent_timeout_secs".into(),
            SettingValue::Uint(u64::from(*secs)),
        ),
        Change::PauseBetweenTasksSecs(secs) => (
            "pause_between_tasks_secs".into(),
            SettingValue::Uint(u64::from(*secs)),
        ),
        Change::PauseBetweenAgentsSecs(secs) => (
            "pause_between_agents_secs".into(),
            SettingValue::Uint(u64::from(*secs)),
        ),
        Change::PauseBetweenCyclesSecs(secs) => (
            "pause_between_cycles_secs".into(),
            SettingValue::Uint(u64::from(*secs)),
        ),
        Change::AdaptivePauses(v) => ("adaptive_pauses".into(), SettingValue::Bool(*v)),
        Change::EngineIdleTimeoutSecs(secs) => (
            "engine_idle_timeout_secs".into(),
            SettingValue::Uint(u64::from(*secs)),
        ),
        // -- Section 7, isolation and security --
        Change::PhaseIsolation(v) => ("phase_isolation".into(), SettingValue::Bool(*v)),
        Change::EnforcePhaseRbac(v) => ("enforce_phase_rbac".into(), SettingValue::Bool(*v)),
        // -- Section 8, learnings, history and embeddings --
        Change::HistoryDir(v) => ("history_dir".into(), str(v)),
        Change::MaxLearningInjection(v) => (
            "max_learning_injection".into(),
            SettingValue::Uint(u64::from(*v)),
        ),
        Change::MinLearningInjection(v) => (
            "min_learning_injection".into(),
            SettingValue::Uint(u64::from(*v)),
        ),
        Change::HistorySearchResults(v) => (
            "history_search_results".into(),
            SettingValue::Uint(u64::from(*v)),
        ),
        Change::HistoryRetentionTasks(v) => (
            "history_retention_tasks".into(),
            SettingValue::Uint(u64::from(*v)),
        ),
        Change::ObservatoryRetentionDays(v) => (
            "observatory_retention_days".into(),
            SettingValue::Uint(u64::from(*v)),
        ),
        Change::SemanticMatchEnabled(v) => {
            ("semantic_match_enabled".into(), SettingValue::Bool(*v))
        }
        Change::EmbeddingModel(v) => ("embedding_model".into(), str(v)),
        Change::EmbeddingPort(port) => (
            "embedding_port".into(),
            SettingValue::Uint(u64::from(*port)),
        ),
        Change::EmbeddingTimeoutMs(ms) => (
            "embedding_timeout_ms".into(),
            SettingValue::Uint(u64::from(*ms)),
        ),
        Change::ShowRetrievalPanel(v) => ("show_retrieval_panel".into(), SettingValue::Bool(*v)),
    }
}

/// The config spelling of a pipeline role, as the `<role>_model` / `<role>_provider`
/// keys are written on disk.
fn role_name(role: Role) -> &'static str {
    match role {
        Role::Unspecified => "unspecified",
        Role::Research => "research",
        Role::Planner => "planner",
        Role::Builder => "builder",
        Role::Reviewer => "reviewer",
        Role::Discovery => "discovery",
        Role::LearningExtraction => "learning_extraction",
        Role::OrchestratorProposer => "orchestrator_proposer",
        Role::OrchestratorReviewer => "orchestrator_reviewer",
    }
}

/// The canonical config spelling of a provider.
fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
        Provider::Opencode => "opencode",
        Provider::Ghcopilot => "ghcopilot",
        Provider::Mistral => "mistral",
    }
}

fn run_mode_name(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Sprint => "sprint",
        RunMode::Continuous => "continuous",
    }
}

fn accept_policy_name(policy: AcceptPolicy) -> &'static str {
    match policy {
        AcceptPolicy::NoHigh => "no-high",
        AcceptPolicy::NoHighMedium => "no-high-medium",
        AcceptPolicy::NoFindings => "no-findings",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patok_proto::{RoleModel, RoleProvider};

    fn maps_to(change: Change, key: &str, value: SettingValue) {
        assert_eq!(from_proto(&change), (key.to_string(), value));
    }

    #[test]
    fn every_case_maps_to_its_registry_key() {
        let bool = |v| SettingValue::Bool(v);
        let uint = |v: u32| SettingValue::Uint(u64::from(v));
        let string = |v: &str| SettingValue::Str(v.into());
        maps_to(
            Change::RoleModel(RoleModel {
                role: Role::Research as i32,
                model: "sonnet".into(),
            }),
            "research_model",
            string("sonnet"),
        );
        maps_to(
            Change::RoleProvider(RoleProvider {
                role: Role::LearningExtraction as i32,
                provider: Provider::Mistral as i32,
            }),
            "learning_extraction_provider",
            string("mistral"),
        );
        // The orchestrator seats map to their names; the registry rejects them uniformly.
        maps_to(
            Change::RoleModel(RoleModel {
                role: Role::OrchestratorProposer as i32,
                model: "m".into(),
            }),
            "orchestrator_proposer_model",
            string("m"),
        );
        maps_to(
            Change::ModelOverride("opus".into()),
            "model",
            string("opus"),
        );
        maps_to(
            Change::LocalModel("local".into()),
            "local_model",
            string("local"),
        );
        maps_to(
            Change::ModelCatalogRefreshSecs(60),
            "model_catalog_refresh_secs",
            uint(60),
        );
        maps_to(
            Change::Provider(Provider::Ghcopilot as i32),
            "provider",
            string("ghcopilot"),
        );
        maps_to(
            Change::RunMode(RunMode::Continuous as i32),
            "run_mode",
            string("continuous"),
        );
        maps_to(
            Change::SkipPlannerForSimple(true),
            "skip_planner_for_simple",
            bool(true),
        );
        maps_to(
            Change::SkipResearchForSimple(true),
            "skip_research_for_simple",
            bool(true),
        );
        maps_to(
            Change::SkipReviewForSimple(true),
            "skip_review_for_simple",
            bool(true),
        );
        maps_to(
            Change::ReviewConfidenceThreshold(5),
            "review_confidence_threshold",
            uint(5),
        );
        maps_to(Change::BatchReview(false), "batch_review", bool(false));
        maps_to(
            Change::PlannerLookahead(true),
            "planner_lookahead",
            bool(true),
        );
        maps_to(
            Change::PlanRevisionCycles(2),
            "plan_revision_cycles",
            uint(2),
        );
        maps_to(
            Change::PlanRevisionAcceptPolicy(AcceptPolicy::NoFindings as i32),
            "plan_revision_accept_policy",
            string("no-findings"),
        );
        maps_to(
            Change::OrchestratorMaxIterations(3),
            "orchestrator_max_iterations",
            uint(3),
        );
        maps_to(
            Change::OrchestratorAcceptPolicy(AcceptPolicy::NoHigh as i32),
            "orchestrator_accept_policy",
            string("no-high"),
        );
        maps_to(Change::ReviewInLoop(true), "review_in_loop", bool(true));
        maps_to(
            Change::ReviewMultipassThreshold(8),
            "review_multipass_threshold",
            uint(8),
        );
        maps_to(
            Change::ConfidenceThreshold(0.5),
            "confidence_threshold",
            SettingValue::Float(0.5),
        );
        maps_to(
            Change::SemgrepEnabled(false),
            "semgrep_enabled",
            bool(false),
        );
        maps_to(
            Change::BuildCommand("make".into()),
            "build_command",
            string("make"),
        );
        maps_to(
            Change::PlanningIterations(1),
            "planning_iterations",
            uint(1),
        );
        maps_to(
            Change::RequireHumanApproval(false),
            "require_human_approval",
            bool(false),
        );
        maps_to(
            Change::AutoPushRemote("origin".into()),
            "auto_push_remote",
            string("origin"),
        );
        maps_to(
            Change::OnTaskCompleteHook("hook".into()),
            "on_task_complete_hook",
            string("hook"),
        );
        maps_to(
            Change::DiscoveryCooldownSecs(300),
            "discovery_cooldown_secs",
            uint(300),
        );
        maps_to(
            Change::DiscoveryCooldownCapSecs(1800),
            "discovery_cooldown_cap_secs",
            uint(1800),
        );
        maps_to(Change::PlanEnabled(false), "plan_enabled", bool(false));
        maps_to(
            Change::AgentTimeoutSecs(120),
            "agent_timeout_secs",
            uint(120),
        );
        maps_to(
            Change::PauseBetweenTasksSecs(10),
            "pause_between_tasks_secs",
            uint(10),
        );
        maps_to(
            Change::PauseBetweenAgentsSecs(3),
            "pause_between_agents_secs",
            uint(3),
        );
        maps_to(
            Change::PauseBetweenCyclesSecs(30),
            "pause_between_cycles_secs",
            uint(30),
        );
        maps_to(Change::AdaptivePauses(true), "adaptive_pauses", bool(true));
        maps_to(
            Change::EngineIdleTimeoutSecs(600),
            "engine_idle_timeout_secs",
            uint(600),
        );
        maps_to(Change::PhaseIsolation(true), "phase_isolation", bool(true));
        maps_to(
            Change::EnforcePhaseRbac(true),
            "enforce_phase_rbac",
            bool(true),
        );
        maps_to(Change::HistoryDir("h".into()), "history_dir", string("h"));
        maps_to(
            Change::MaxLearningInjection(10),
            "max_learning_injection",
            uint(10),
        );
        maps_to(
            Change::MinLearningInjection(2),
            "min_learning_injection",
            uint(2),
        );
        maps_to(
            Change::HistorySearchResults(5),
            "history_search_results",
            uint(5),
        );
        maps_to(
            Change::HistoryRetentionTasks(50),
            "history_retention_tasks",
            uint(50),
        );
        maps_to(
            Change::ObservatoryRetentionDays(30),
            "observatory_retention_days",
            uint(30),
        );
        maps_to(
            Change::SemanticMatchEnabled(true),
            "semantic_match_enabled",
            bool(true),
        );
        maps_to(
            Change::EmbeddingModel("m".into()),
            "embedding_model",
            string("m"),
        );
        maps_to(Change::EmbeddingPort(11434), "embedding_port", uint(11434));
        maps_to(
            Change::EmbeddingTimeoutMs(2000),
            "embedding_timeout_ms",
            uint(2000),
        );
        maps_to(
            Change::ShowRetrievalPanel(false),
            "show_retrieval_panel",
            bool(false),
        );
        // Empty optional strings clear the key.
        maps_to(Change::ModelOverride(String::new()), "model", string(""));
        maps_to(
            Change::BuildCommand(String::new()),
            "build_command",
            string(""),
        );
    }
}
