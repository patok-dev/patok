//! The settings-change wire mapping, in both directions, generated from one list
//! (finding 10 of the structure review).
//!
//! `from_proto` (the engine direction: a wire change onto its `[daemon]` key and
//! value) and `to_proto` (the shell direction: a key and value onto the typed wire
//! case) used to be two hand-maintained matches in two crates, so every setting
//! needed a hand-written arm in each and the two could drift apart. Both are
//! generated here from a single key-to-case list, the `daemon_wire_table!`
//! invocation below: the `from_proto` match is exhaustive over the proto oneof, so a
//! proto case without a list line fails the build; the cross-crate drift test in
//! `patok-tui` (`every_daemon_field_has_an_overlay_row_and_round_trips_the_wire`)
//! closes the other side, asserting every core registry field round-trips its key
//! and value through both mappers. The agent-role table and the proto/core enum
//! bridges (provider, run mode, accept policy) live here too, in one place instead
//! of mirrored across the engine and the shell.
//!
//! Adding a daemon setting is the `daemon_table!` entry in `patok-core`, the proto
//! oneof field, and one line in the list below -- nothing else on the wire.

use patok_core::config::{
    AcceptPolicy as CoreAcceptPolicy, EnumSpelling, ProviderName, RunMode as CoreRunMode,
    SettingValue,
};
use patok_proto::settings_change::Change;
use patok_proto::{AcceptPolicy, Provider, Role, RoleModel, RoleProvider, RunMode};

/// The agent-role table, generated from one list: the config spelling of every proto
/// role (`role_name`, the `<role>_model` / `<role>_provider` key prefix direction)
/// and its inverse `role_of`. All nine roles carry a name so the orchestrator seats
/// and `Unspecified` flow through `from_proto` onto unregistered keys and the
/// persist layer rejects them uniformly; `role_of` answers only the prefixes the
/// registry keys slice off, so the extra spellings are unobservable there.
macro_rules! wire_roles {
    (name [$($name_acc:tt)*] of [$($of_acc:tt)*]) => {
        /// The config spelling of a pipeline role, as the `<role>_model` /
        /// `<role>_provider` keys are written on disk.
        fn role_name(role: Role) -> &'static str {
            match role {
                $($name_acc)*
            }
        }

        /// The proto role of a config role spelling, the prefix of a
        /// `<role>_model` / `<role>_provider` key.
        fn role_of(name: &str) -> Result<Role, String> {
            match name {
                $($of_acc)*
                _ => Err(format!("unknown agent role `{name}`")),
            }
        }
    };
    (
        name [$($name_acc:tt)*] of [$($of_acc:tt)*]
        [$variant:ident $spelling:literal]
        $($rest:tt)*
    ) => {
        wire_roles! {
            name [$($name_acc)* Role::$variant => $spelling,]
            of [$($of_acc)* $spelling => Ok(Role::$variant),]
            $($rest)*
        }
    };
}

wire_roles! {
    name [] of []
    [Unspecified "unspecified"]
    [Research "research"]
    [Planner "planner"]
    [Builder "builder"]
    [Reviewer "reviewer"]
    [Discovery "discovery"]
    [LearningExtraction "learning_extraction"]
    [Orchestrator "orchestrator"]
    [OrchestratorProposer "orchestrator_proposer"]
    [OrchestratorReviewer "orchestrator_reviewer"]
}

/// The enum value of a raw wire number; the zero value when the number is outside the
/// enum (only possible from a mismatched client, since this is an internal protocol).
fn of<T>(raw: i32) -> T
where
    T: Default + TryFrom<i32>,
{
    T::try_from(raw).unwrap_or_default()
}

/// The core config enum of a wire provider, whose `as_str` supplies the spelling.
fn core_provider(provider: Provider) -> ProviderName {
    match provider {
        Provider::Claude => ProviderName::Claude,
        Provider::Codex => ProviderName::Codex,
        Provider::Opencode => ProviderName::OpenCode,
        Provider::Ghcopilot => ProviderName::GhCopilot,
        Provider::Mistral => ProviderName::Mistral,
    }
}

/// The canonical config spelling of a provider.
fn provider_name(provider: Provider) -> &'static str {
    core_provider(provider).as_str()
}

/// The core config enum of a wire run mode, whose `as_str` supplies the spelling.
fn core_run_mode(mode: RunMode) -> CoreRunMode {
    match mode {
        RunMode::Sprint => CoreRunMode::Sprint,
        RunMode::Continuous => CoreRunMode::Continuous,
    }
}

fn run_mode_name(mode: RunMode) -> &'static str {
    core_run_mode(mode).as_str()
}

/// The core config enum of a wire accept policy, whose `as_str` supplies the
/// spelling.
fn core_accept_policy(policy: AcceptPolicy) -> CoreAcceptPolicy {
    match policy {
        AcceptPolicy::NoHigh => CoreAcceptPolicy::NoHigh,
        AcceptPolicy::NoHighMedium => CoreAcceptPolicy::NoHighMedium,
        AcceptPolicy::NoFindings => CoreAcceptPolicy::NoFindings,
    }
}

fn accept_policy_name(policy: AcceptPolicy) -> &'static str {
    core_accept_policy(policy).as_str()
}

/// The wire enum of a core run mode.
fn proto_run_mode(mode: CoreRunMode) -> RunMode {
    match mode {
        CoreRunMode::Sprint => RunMode::Sprint,
        CoreRunMode::Continuous => RunMode::Continuous,
    }
}

/// The wire enum of a core accept policy.
fn proto_accept_policy(policy: CoreAcceptPolicy) -> AcceptPolicy {
    match policy {
        CoreAcceptPolicy::NoHigh => AcceptPolicy::NoHigh,
        CoreAcceptPolicy::NoHighMedium => AcceptPolicy::NoHighMedium,
        CoreAcceptPolicy::NoFindings => AcceptPolicy::NoFindings,
    }
}

/// The wire enum of a provider's canonical spelling; unknown text is a
/// user-facing rejection. Only canonical spellings travel the wire (aliases
/// are a TOML-only concern), so this uses the trait's canonical parse, not
/// the alias-aware `ProviderName::parse`.
fn provider_of(name: &str) -> Result<Provider, String> {
    <ProviderName as EnumSpelling>::parse(name)
        .map(proto_provider)
        .ok_or_else(|| format!("`provider` must be a provider name, got {name:?}"))
}

/// The wire enum of a core provider name.
fn proto_provider(name: ProviderName) -> Provider {
    match name {
        ProviderName::Claude => Provider::Claude,
        ProviderName::Codex => Provider::Codex,
        ProviderName::OpenCode => Provider::Opencode,
        ProviderName::GhCopilot => Provider::Ghcopilot,
        ProviderName::Mistral => Provider::Mistral,
    }
}

/// The `from_proto` match arms, accumulated one list entry at a time: each step
/// appends the entry's match arm (per-role entries contribute none -- the two role
/// arms carry no per-entry data and are written once in the base arm) and recurses;
/// the base arm emits the whole match, exhaustive over the proto oneof.
///
/// The entry shapes name what the wire case carries: `bool`, `uint`, `float`, `text`,
/// `provider`, `run_mode` and `accept_policy` (each with its key literal and proto
/// case), and the two per-role shapes `role_model` / `role_provider` with their key
/// literals. A proto case missing from the list is a non-exhaustive match: a compile
/// error, not a silent gap.
macro_rules! wire_from_proto {
    ($str:ident, $change:ident, arms [$($acc:tt)*]) => {
        match $change {
            Change::RoleModel(role) => (
                format!("{}_model", role_name(of(role.role))),
                $str(&role.model),
            ),
            Change::RoleProvider(role) => (
                format!("{}_provider", role_name(of(role.role))),
                $str(provider_name(of(role.provider))),
            ),
            $($acc)*
        }
    };
    (
        $str:ident, $change:ident, arms [$($acc:tt)*]
        [bool $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_from_proto! {
            $str, $change,
            arms [$($acc)* Change::$case(v) => ($k.into(), SettingValue::Bool(*v)),]
            $($rest)*
        }
    };
    (
        $str:ident, $change:ident, arms [$($acc:tt)*]
        [uint $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_from_proto! {
            $str, $change,
            arms [$($acc)* Change::$case(v) => ($k.into(), SettingValue::Uint(u64::from(*v))),]
            $($rest)*
        }
    };
    (
        $str:ident, $change:ident, arms [$($acc:tt)*]
        [float $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_from_proto! {
            $str, $change,
            arms [$($acc)* Change::$case(v) => ($k.into(), SettingValue::Float(*v)),]
            $($rest)*
        }
    };
    (
        $str:ident, $change:ident, arms [$($acc:tt)*]
        [text $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_from_proto! {
            $str, $change,
            arms [$($acc)* Change::$case(v) => ($k.into(), $str(v)),]
            $($rest)*
        }
    };
    (
        $str:ident, $change:ident, arms [$($acc:tt)*]
        [provider $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_from_proto! {
            $str, $change,
            arms [$($acc)* Change::$case(v) => ($k.into(), $str(provider_name(of(*v)))),]
            $($rest)*
        }
    };
    (
        $str:ident, $change:ident, arms [$($acc:tt)*]
        [run_mode $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_from_proto! {
            $str, $change,
            arms [$($acc)* Change::$case(v) => ($k.into(), $str(run_mode_name(of(*v)))),]
            $($rest)*
        }
    };
    (
        $str:ident, $change:ident, arms [$($acc:tt)*]
        [accept_policy $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_from_proto! {
            $str, $change,
            arms [$($acc)* Change::$case(v) => ($k.into(), $str(accept_policy_name(of(*v)))),]
            $($rest)*
        }
    };
    // The per-role entries carry no per-entry data: the two arms above cover them.
    (
        $str:ident, $change:ident, arms [$($acc:tt)*]
        [role_model $k:literal] $($rest:tt)*
    ) => {
        wire_from_proto! { $str, $change, arms [$($acc)*] $($rest)* }
    };
    (
        $str:ident, $change:ident, arms [$($acc:tt)*]
        [role_provider $k:literal] $($rest:tt)*
    ) => {
        wire_from_proto! { $str, $change, arms [$($acc)*] $($rest)* }
    };
}

/// The `to_proto` match arms, accumulated one list entry at a time: each step
/// appends the entry's key-matched arm (the per-role arms slice the role prefix off
/// the key, so the key and its role cannot disagree) and recurses; the base arm
/// emits the whole match with the catch-all rejection. Only the registered entries
/// reach this muncher, so the deliberately unregistered keys stay rejected with
/// "no settings-change case" exactly as before.
macro_rules! wire_to_proto {
    (
        $field:ident, $value:ident, $boolean:ident, $uint:ident, $string:ident, $wrong:ident,
        arms [$($acc:tt)*]
    ) => {
        match $field {
            $($acc)*
            other => return Err(format!("no settings-change case for `{other}`")),
        }
    };
    (
        $field:ident, $value:ident, $boolean:ident, $uint:ident, $string:ident, $wrong:ident,
        arms [$($acc:tt)*]
        [bool $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_to_proto! {
            $field, $value, $boolean, $uint, $string, $wrong,
            arms [$($acc)* $k => Change::$case($boolean()?),]
            $($rest)*
        }
    };
    (
        $field:ident, $value:ident, $boolean:ident, $uint:ident, $string:ident, $wrong:ident,
        arms [$($acc:tt)*]
        [uint $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_to_proto! {
            $field, $value, $boolean, $uint, $string, $wrong,
            arms [$($acc)* $k => Change::$case($uint()?),]
            $($rest)*
        }
    };
    (
        $field:ident, $value:ident, $boolean:ident, $uint:ident, $string:ident, $wrong:ident,
        arms [$($acc:tt)*]
        [float $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_to_proto! {
            $field, $value, $boolean, $uint, $string, $wrong,
            arms [$($acc)*
                $k => Change::$case(match $value {
                    SettingValue::Float(f) => f,
                    _ => return $wrong("a number"),
                }),
            ]
            $($rest)*
        }
    };
    (
        $field:ident, $value:ident, $boolean:ident, $uint:ident, $string:ident, $wrong:ident,
        arms [$($acc:tt)*]
        [text $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_to_proto! {
            $field, $value, $boolean, $uint, $string, $wrong,
            arms [$($acc)* $k => Change::$case($string()?),]
            $($rest)*
        }
    };
    (
        $field:ident, $value:ident, $boolean:ident, $uint:ident, $string:ident, $wrong:ident,
        arms [$($acc:tt)*]
        [provider $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_to_proto! {
            $field, $value, $boolean, $uint, $string, $wrong,
            arms [$($acc)* $k => Change::$case(provider_of(&$string()?)? as i32),]
            $($rest)*
        }
    };
    (
        $field:ident, $value:ident, $boolean:ident, $uint:ident, $string:ident, $wrong:ident,
        arms [$($acc:tt)*]
        [run_mode $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_to_proto! {
            $field, $value, $boolean, $uint, $string, $wrong,
            arms [$($acc)*
                $k => {
                    let mode = CoreRunMode::parse(&$string()?).ok_or_else(|| {
                        format!("`{}` must be one of {}", $k, CoreRunMode::accepted_spellings())
                    })?;
                    Change::$case(proto_run_mode(mode) as i32)
                },
            ]
            $($rest)*
        }
    };
    (
        $field:ident, $value:ident, $boolean:ident, $uint:ident, $string:ident, $wrong:ident,
        arms [$($acc:tt)*]
        [accept_policy $k:literal $case:ident] $($rest:tt)*
    ) => {
        wire_to_proto! {
            $field, $value, $boolean, $uint, $string, $wrong,
            arms [$($acc)*
                $k => {
                    let policy = CoreAcceptPolicy::parse(&$string()?).ok_or_else(|| {
                        format!(
                            "`{}` must be one of {}",
                            $k,
                            CoreAcceptPolicy::accepted_spellings()
                        )
                    })?;
                    Change::$case(proto_accept_policy(policy) as i32)
                },
            ]
            $($rest)*
        }
    };
    (
        $field:ident, $value:ident, $boolean:ident, $uint:ident, $string:ident, $wrong:ident,
        arms [$($acc:tt)*]
        [role_model $k:literal] $($rest:tt)*
    ) => {
        wire_to_proto! {
            $field, $value, $boolean, $uint, $string, $wrong,
            arms [$($acc)*
                $k => Change::RoleModel(RoleModel {
                    role: role_of(&$field[..$field.len() - 6])? as i32,
                    model: $string()?,
                }),
            ]
            $($rest)*
        }
    };
    (
        $field:ident, $value:ident, $boolean:ident, $uint:ident, $string:ident, $wrong:ident,
        arms [$($acc:tt)*]
        [role_provider $k:literal] $($rest:tt)*
    ) => {
        wire_to_proto! {
            $field, $value, $boolean, $uint, $string, $wrong,
            arms [$($acc)*
                $k => Change::RoleProvider(RoleProvider {
                    role: role_of(&$field[..$field.len() - 9])? as i32,
                    provider: provider_of(&$string()?)? as i32,
                }),
            ]
            $($rest)*
        }
    };
}

/// The settings-change wire table: the one key-to-case list both directions are
/// generated from. The `registered` entries are exactly the core registry's
/// single-value keys, so the shell can commit every one of them; the
/// `unregistered` section lists the proto cases that deliberately name keys
/// outside the registry (the orchestrator seats' settings), which map in
/// `from_proto` -- the persist layer then rejects them with the same "unknown
/// daemon setting" error as any other unrecognized field -- but stay rejected by
/// `to_proto`, since the overlay has no row to submit them from.
macro_rules! daemon_wire_table {
    (
        registered { $($reg:tt)* }
        unregistered { $($un:tt)* }
    ) => {
        /// The on-disk `[daemon]` key and the submitted value of one settings change.
        /// Every case maps to its registry name; names absent from the registry (the
        /// orchestrator seats) flow on uniformly so the persist layer rejects them with
        /// the same "unknown daemon setting" error as any other unrecognized field. The
        /// match is exhaustive over the proto oneof, and the cross-crate drift test in
        /// `patok-tui` (`every_daemon_field_has_an_overlay_row_and_round_trips_the_wire`)
        /// asserts every core registry field round-trips its key and value through the
        /// shell's `to_proto` and this function, so the two mappers cannot drift apart
        /// silently.
        pub fn from_proto(change: &Change) -> (String, SettingValue) {
            let str = |value: &str| SettingValue::Str(value.to_string());
            wire_from_proto! { str, change, arms [] $($reg)* $($un)* }
        }

        /// Maps one daemon-schema settings change onto its typed wire case, the exact
        /// inverse of `from_proto`: every overlay row a shell can commit has a case
        /// here; names without one are a programming error on the shell side. The
        /// value's shape is checked against the case, not just the registry, so a
        /// mismatch is a user-facing rejection rather than a wire corruption.
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
            Ok(wire_to_proto! {
                field, value, boolean, uint, string, wrong,
                arms [] $($reg)*
            })
        }
    };
}

daemon_wire_table! {
    registered {
        // Model and provider routing (proto section 3.4).
        [role_model "research_model"]
        [role_model "planner_model"]
        [role_model "builder_model"]
        [role_model "reviewer_model"]
        [role_model "discovery_model"]
        [role_model "orchestrator_model"]
        [role_provider "research_provider"]
        [role_provider "planner_provider"]
        [role_provider "builder_provider"]
        [role_provider "reviewer_provider"]
        [role_provider "discovery_provider"]
        [role_provider "orchestrator_provider"]
        [text "local_model" LocalModel]
        [uint "model_catalog_refresh_secs" ModelCatalogRefreshSecs]
        // Run modes and pipeline flow (proto section 3.5).
        [run_mode "run_mode" RunMode]
        [bool "skip_planner_for_simple" SkipPlannerForSimple]
        [bool "skip_research_for_simple" SkipResearchForSimple]
        [bool "skip_review_for_simple" SkipReviewForSimple]
        [uint "review_confidence_threshold" ReviewConfidenceThreshold]
        [bool "batch_review" BatchReview]
        [bool "planner_lookahead" PlannerLookahead]
        [bool "review_in_loop" ReviewInLoop]
        [uint "review_multipass_threshold" ReviewMultipassThreshold]
        [float "confidence_threshold" ConfidenceThreshold]
        [uint "plan_revision_cycles" PlanRevisionCycles]
        [accept_policy "plan_revision_accept_policy" PlanRevisionAcceptPolicy]
        [bool "semgrep_enabled" SemgrepEnabled]
        [text "build_command" BuildCommand]
        [uint "planning_iterations" PlanningIterations]
        [bool "require_human_approval" RequireHumanApproval]
        [text "on_task_complete_hook" OnTaskCompleteHook]
        [bool "plan_enabled" PlanEnabled]
        [uint "discovery_cooldown_secs" DiscoveryCooldownSecs]
        [uint "discovery_cooldown_cap_secs" DiscoveryCooldownCapSecs]
        // Timing (proto section 3.6).
        [uint "agent_timeout_secs" AgentTimeoutSecs]
        [uint "pause_between_tasks_secs" PauseBetweenTasksSecs]
        [uint "pause_between_agents_secs" PauseBetweenAgentsSecs]
        [uint "pause_between_cycles_secs" PauseBetweenCyclesSecs]
        [bool "adaptive_pauses" AdaptivePauses]
        [uint "engine_idle_timeout_secs" EngineIdleTimeoutSecs]
        // Isolation and security (proto section 3.7).
        [bool "phase_isolation" PhaseIsolation]
        [bool "enforce_phase_rbac" EnforcePhaseRbac]
        // Learnings, history and embeddings (proto section 3.8).
        [text "history_dir" HistoryDir]
        [uint "max_learning_injection" MaxLearningInjection]
        [uint "min_learning_injection" MinLearningInjection]
        [uint "history_search_results" HistorySearchResults]
        [uint "history_retention_tasks" HistoryRetentionTasks]
        [uint "history_retention_days" HistoryRetentionDays]
        [uint "observatory_retention_days" ObservatoryRetentionDays]
        [bool "semantic_match_enabled" SemanticMatchEnabled]
        [text "embedding_model" EmbeddingModel]
        [uint "embedding_port" EmbeddingPort]
        [uint "embedding_timeout_ms" EmbeddingTimeoutMs]
        [bool "show_retrieval_panel" ShowRetrievalPanel]
        // Git.
        [text "auto_push_remote" AutoPushRemote]
    }
    unregistered {
        // Removed global settings: proto cases for the removed global provider and model
        // fields; `from_proto` rejects them with a "no settings-change case" error.
        [provider "provider" Provider]
        [text "model" ModelOverride]
        // The orchestrator seats' settings: proto cases whose keys are deliberately
        // outside the core registry; `from_proto` maps them so the persist layer
        // rejects them uniformly, `to_proto` keeps rejecting the submit.
        [uint "orchestrator_max_iterations" OrchestratorMaxIterations]
        [accept_policy "orchestrator_accept_policy" OrchestratorAcceptPolicy]
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
                role: Role::Orchestrator as i32,
                provider: Provider::Mistral as i32,
            }),
            "orchestrator_provider",
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
            Change::HistoryRetentionTasks(1000),
            "history_retention_tasks",
            uint(1000),
        );
        maps_to(
            Change::HistoryRetentionDays(30),
            "history_retention_days",
            uint(30),
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

    /// The inverse of `from_proto`: every case this shell can produce maps
    /// back onto its registry key, so the two ends agree at compile time.
    #[test]
    fn to_proto_maps_every_overlay_field_onto_its_wire_case() {
        let str = |v: &str| SettingValue::Str(v.into());
        let uint = |v: u64| SettingValue::Uint(v);
        // The removed global keys have no overlay field, so the shell cannot submit them.
        assert!(to_proto("provider", str("mistral")).is_err());
        assert!(to_proto("model", str("opus")).is_err());
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
                Change::RoleModel(RoleModel {
                    role: role_of(role).unwrap() as i32,
                    model: model.into(),
                }),
                "{role}_model"
            );
            assert_eq!(
                to_proto(&format!("{role}_provider"), str("mistral")).unwrap(),
                Change::RoleProvider(RoleProvider {
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
        // An empty value switches the push off.
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
        let error = to_proto(
            "builder_provider",
            SettingValue::Str("not-a-provider".into()),
        )
        .unwrap_err();
        assert!(error.contains("provider name"), "{error}");
        // The alias is a TOML-only concern: only canonical spellings travel the wire.
        let error = to_proto("builder_provider", SettingValue::Str("vibe".into())).unwrap_err();
        assert!(error.contains("provider name"), "{error}");
        let error = to_proto("research_model", SettingValue::Bool(true)).unwrap_err();
        assert!(error.contains("must be a string"), "{error}");
        // The orchestrator keys are deliberately outside the registry: a submit of
        // one stays rejected with the catch-all message.
        let error = to_proto("orchestrator_max_iterations", SettingValue::Uint(1)).unwrap_err();
        assert!(error.contains("no settings-change case"), "{error}");
    }
}
