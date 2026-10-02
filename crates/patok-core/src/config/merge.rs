//! The deep merge of the file layers, applied independently
//! per schema: a higher-precedence value at a given key overrides the lower one, keys a
//! file omits keep the lower layer's value. Structured fields merge deeper: the stage
//! list merges by stage id (only the fields a higher entry sets), the catalog URL
//! override map merges per key, and plain lists are replaced wholesale.

use std::collections::BTreeMap;

use super::daemon::{DaemonLayer, RawStage};
use super::tui::TuiLayer;

/// Merges two daemon layers, `higher` winning per key.
pub(crate) fn daemon(lower: DaemonLayer, higher: DaemonLayer) -> DaemonLayer {
    DaemonLayer {
        provider: higher.provider.or(lower.provider),
        model: higher.model.or(lower.model),
        research_model: higher.research_model.or(lower.research_model),
        planner_model: higher.planner_model.or(lower.planner_model),
        builder_model: higher.builder_model.or(lower.builder_model),
        reviewer_model: higher.reviewer_model.or(lower.reviewer_model),
        discovery_model: higher.discovery_model.or(lower.discovery_model),
        learning_extraction_model: higher
            .learning_extraction_model
            .or(lower.learning_extraction_model),
        research_provider: higher.research_provider.or(lower.research_provider),
        planner_provider: higher.planner_provider.or(lower.planner_provider),
        builder_provider: higher.builder_provider.or(lower.builder_provider),
        reviewer_provider: higher.reviewer_provider.or(lower.reviewer_provider),
        discovery_provider: higher.discovery_provider.or(lower.discovery_provider),
        learning_extraction_provider: higher
            .learning_extraction_provider
            .or(lower.learning_extraction_provider),
        local_model: higher.local_model.or(lower.local_model),
        model_catalog_refresh_secs: higher
            .model_catalog_refresh_secs
            .or(lower.model_catalog_refresh_secs),
        catalog_url_overrides: merge_maps(
            lower.catalog_url_overrides,
            higher.catalog_url_overrides,
        ),
        stage_overrides: higher.stage_overrides.or(lower.stage_overrides),
        run_mode: higher.run_mode.or(lower.run_mode),
        stages: merge_stages(lower.stages, higher.stages),
        skip_planner_for_simple: higher
            .skip_planner_for_simple
            .or(lower.skip_planner_for_simple),
        skip_research_for_simple: higher
            .skip_research_for_simple
            .or(lower.skip_research_for_simple),
        skip_review_for_simple: higher
            .skip_review_for_simple
            .or(lower.skip_review_for_simple),
        review_confidence_threshold: higher
            .review_confidence_threshold
            .or(lower.review_confidence_threshold),
        batch_review: higher.batch_review.or(lower.batch_review),
        planner_lookahead: higher.planner_lookahead.or(lower.planner_lookahead),
        plan_revision_cycles: higher.plan_revision_cycles.or(lower.plan_revision_cycles),
        plan_revision_accept_policy: higher
            .plan_revision_accept_policy
            .or(lower.plan_revision_accept_policy),
        review_in_loop: higher.review_in_loop.or(lower.review_in_loop),
        review_multipass_threshold: higher
            .review_multipass_threshold
            .or(lower.review_multipass_threshold),
        confidence_threshold: higher.confidence_threshold.or(lower.confidence_threshold),
        semgrep_enabled: higher.semgrep_enabled.or(lower.semgrep_enabled),
        semgrep_rulesets: higher.semgrep_rulesets.or(lower.semgrep_rulesets),
        build_command: higher.build_command.or(lower.build_command),
        planning_iterations: higher.planning_iterations.or(lower.planning_iterations),
        require_human_approval: higher
            .require_human_approval
            .or(lower.require_human_approval),
        auto_push_remote: higher.auto_push_remote.or(lower.auto_push_remote),
        on_task_complete_hook: higher.on_task_complete_hook.or(lower.on_task_complete_hook),
        discovery_cooldown_secs: higher
            .discovery_cooldown_secs
            .or(lower.discovery_cooldown_secs),
        discovery_cooldown_cap_secs: higher
            .discovery_cooldown_cap_secs
            .or(lower.discovery_cooldown_cap_secs),
        plan_enabled: higher.plan_enabled.or(lower.plan_enabled),
        agent_timeout_secs: higher.agent_timeout_secs.or(lower.agent_timeout_secs),
        pause_between_tasks_secs: higher
            .pause_between_tasks_secs
            .or(lower.pause_between_tasks_secs),
        pause_between_agents_secs: higher
            .pause_between_agents_secs
            .or(lower.pause_between_agents_secs),
        pause_between_cycles_secs: higher
            .pause_between_cycles_secs
            .or(lower.pause_between_cycles_secs),
        adaptive_pauses: higher.adaptive_pauses.or(lower.adaptive_pauses),
        engine_idle_timeout_secs: higher
            .engine_idle_timeout_secs
            .or(lower.engine_idle_timeout_secs),
        phase_isolation: higher.phase_isolation.or(lower.phase_isolation),
        enforce_phase_rbac: higher.enforce_phase_rbac.or(lower.enforce_phase_rbac),
        history_dir: higher.history_dir.or(lower.history_dir),
        max_learning_injection: higher
            .max_learning_injection
            .or(lower.max_learning_injection),
        min_learning_injection: higher
            .min_learning_injection
            .or(lower.min_learning_injection),
        history_search_results: higher
            .history_search_results
            .or(lower.history_search_results),
        history_retention_tasks: higher
            .history_retention_tasks
            .or(lower.history_retention_tasks),
        observatory_retention_days: higher
            .observatory_retention_days
            .or(lower.observatory_retention_days),
        semantic_match_enabled: higher
            .semantic_match_enabled
            .or(lower.semantic_match_enabled),
        embedding_model: higher.embedding_model.or(lower.embedding_model),
        embedding_port: higher.embedding_port.or(lower.embedding_port),
        embedding_timeout_ms: higher.embedding_timeout_ms.or(lower.embedding_timeout_ms),
        plugins: higher.plugins.or(lower.plugins),
        show_retrieval_panel: higher.show_retrieval_panel.or(lower.show_retrieval_panel),
    }
}

/// Merges two tui layers, `higher` winning per key.
pub(crate) fn tui(lower: TuiLayer, higher: TuiLayer) -> TuiLayer {
    TuiLayer {
        theme: higher.theme.or(lower.theme),
        truecolor: higher.truecolor.or(lower.truecolor),
        preview_wrap: higher.preview_wrap.or(lower.preview_wrap),
        agent_pane_split: higher.agent_pane_split.or(lower.agent_pane_split),
        update_channel: higher.update_channel.or(lower.update_channel),
        rail_mode: higher.rail_mode.or(lower.rail_mode),
    }
}

/// The stage list merges by stage id: a higher entry
/// overrides only that stage's fields, a stage a file omits keeps the lower definition,
/// and a stage id a higher layer introduces is appended after the lower list.
fn merge_stages(
    lower: Option<Vec<RawStage>>,
    higher: Option<Vec<RawStage>>,
) -> Option<Vec<RawStage>> {
    match (lower, higher) {
        (None, None) => None,
        (Some(lower), None) => Some(lower),
        (lower, Some(higher)) => {
            let mut merged = lower.unwrap_or_default();
            for stage in higher {
                if let Some(existing) = merged.iter_mut().find(|s| s.id == stage.id) {
                    existing.label = stage.label.or(existing.label.take());
                    existing.enabled = stage.enabled.or(existing.enabled.take());
                    existing.prompt = stage.prompt.or(existing.prompt.take());
                } else {
                    merged.push(stage);
                }
            }
            Some(merged)
        }
    }
}

/// A string map merges per key: keys a higher layer omits keep the lower value.
fn merge_maps(
    lower: Option<BTreeMap<String, String>>,
    higher: Option<BTreeMap<String, String>>,
) -> Option<BTreeMap<String, String>> {
    match (lower, higher) {
        (None, None) => None,
        (Some(lower), None) => Some(lower),
        (lower, Some(higher)) => {
            let mut merged = lower.unwrap_or_default();
            merged.extend(higher);
            Some(merged)
        }
    }
}
