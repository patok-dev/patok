//! Start-up model validation: a background check that the configured models are among the
//! ones the configured harnesses report. Warn-only; it never delays or fails startup.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use patok_config::DaemonSettings;
use patok_providers::{
    DiscoveredModel, ModelCache, Provider, ProviderError, ProviderKind, ProviderName,
};
use tokio::task::{JoinHandle, JoinSet};

use super::Engine;
use super::state::Resolver;

/// How long one harness gets to report its models.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// The six (field, provider, model) role settings, in the order of
/// `model_selection_warnings` in `patok-config`, which this mirrors.
fn role_models(s: &DaemonSettings) -> [(&'static str, ProviderName, &String); 6] {
    [
        ("research_model", s.research_provider, &s.research_model),
        ("planner_model", s.planner_provider, &s.planner_model),
        ("builder_model", s.builder_provider, &s.builder_model),
        ("reviewer_model", s.reviewer_provider, &s.reviewer_model),
        ("discovery_model", s.discovery_provider, &s.discovery_model),
        (
            "orchestrator_model",
            s.orchestrator_provider,
            &s.orchestrator_model,
        ),
    ]
}

/// `id` without one trailing `[...]` suffix (`sonnet[1m]` -> `sonnet`).
fn base_id(id: &str) -> &str {
    match id
        .strip_suffix(']')
        .and_then(|rest| rest.rfind('[').map(|i| &id[..i]))
    {
        Some(base) => base,
        None => id,
    }
}

/// Whether `model` matches a discovered model, ignoring `[...]` suffixes on both sides.
/// Unlisted (hidden) models count as known.
fn is_known(model: &str, discovered: &[DiscoveredModel]) -> bool {
    discovered.iter().any(|m| base_id(&m.id) == base_id(model))
}

/// The models of `provider`: the fresh cache entry when there is one, else a discovery
/// that refreshes the cache. Without a harness version nothing is read from or written
/// to the cache.
async fn models_of(
    provider: Arc<dyn Provider>,
    cache: Option<ModelCache>,
) -> Result<Vec<DiscoveredModel>, ProviderError> {
    let slug = provider.slug();
    let version = provider.harness_version().await;
    let keyed = match (&cache, &version) {
        (Some(cache), Ok(version)) => Some((cache, version.as_str())),
        _ => None,
    };
    if let Err(error) = &version {
        tracing::debug!(provider = slug, %error, "no harness version; the model cache is skipped");
    }
    if let Some((cache, version)) = keyed
        && let Some(models) = cache.load(slug, version, SystemTime::now())
    {
        return Ok(models);
    }
    let models = provider.discover_models().await?;
    if let Some((cache, version)) = keyed
        && let Err(error) = cache.store(slug, version, &models, SystemTime::now())
    {
        tracing::debug!(provider = slug, %error, "cannot store the model cache");
    }
    Ok(models)
}

/// The warnings for the configured models that the configured harnesses do not report.
///
/// Only providers that appear in a `*_provider` setting and support model selection are
/// asked (Vibe never is), concurrently and each within `timeout`. A harness that cannot
/// be asked yields a "not checked" warning instead of unknown-model warnings.
pub(crate) async fn validate_models(
    settings: &DaemonSettings,
    resolver: &Resolver,
    cache: Option<&ModelCache>,
    timeout: Duration,
) -> Vec<String> {
    let mut kinds: Vec<ProviderKind> = vec![];
    for kind in role_models(settings)
        .into_iter()
        .filter_map(|(_, provider, _)| provider.kind())
        .filter(|kind| kind.info().supports_model_selection)
    {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }

    let mut outcomes: Vec<(ProviderKind, Result<Vec<DiscoveredModel>, String>)> = vec![];
    let mut jobs = JoinSet::new();
    for kind in kinds.iter().copied() {
        match resolver(kind) {
            Err(error) => outcomes.push((kind, Err(error.to_string()))),
            Ok(provider) => {
                let cache = cache.cloned();
                jobs.spawn(async move {
                    (
                        kind,
                        tokio::time::timeout(timeout, models_of(provider, cache)).await,
                    )
                });
            }
        }
    }
    while let Some(joined) = jobs.join_next().await {
        // A join error means the task panicked or was aborted; there is nothing to report.
        let Ok((kind, result)) = joined else { continue };
        let result = match result {
            Ok(Ok(models)) => Ok(models),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err(format!("discovery timed out after {} s", timeout.as_secs())),
        };
        outcomes.push((kind, result));
    }

    let mut warnings = vec![];
    for kind in &kinds {
        if let Some((_, Err(reason))) = outcomes.iter().find(|(k, _)| k == kind) {
            warnings.push(format!(
                "{} models not checked: {reason}",
                kind.info().harness_name
            ));
        }
    }
    for (field, provider, model) in role_models(settings) {
        let Some(kind) = provider.kind() else {
            continue;
        };
        let Some((_, Ok(discovered))) = outcomes.iter().find(|(k, _)| *k == kind) else {
            continue;
        };
        if !model.is_empty() && !is_known(model, discovered) {
            let harness = kind.info().harness_name;
            warnings.push(format!(
                "`{field} = {model:?}` is not among the models {harness} reports; it is passed to {harness} unchanged"
            ));
        }
    }
    warnings
}

impl Engine {
    /// Checks the configured models against the harnesses' reported models in a background
    /// task and logs a warning for each unknown one. It never blocks or fails startup, reads
    /// the settings as of this call, and stops on shutdown: cancellation drops the discovery
    /// futures, which kills the harness children. The handle yields the warnings (empty on
    /// engines without a resolver and when cancelled).
    pub fn spawn_model_validation(&self, cache: Option<ModelCache>) -> JoinHandle<Vec<String>> {
        let settings = self.settings();
        let resolver = self.inner.resolver.clone();
        let shutdown = self.inner.shutdown.clone();
        tokio::spawn(async move {
            let Some(resolver) = resolver else {
                return vec![];
            };
            tokio::select! {
                biased;
                () = shutdown.cancelled() => vec![],
                warnings = validate_models(&settings, &resolver, cache.as_ref(), DISCOVERY_TIMEOUT) => {
                    for warning in &warnings {
                        tracing::warn!("{warning}");
                    }
                    warnings
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use patok_providers::mock::MockProvider;

    use super::*;

    fn model(id: &str) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            display_name: None,
            listed: true,
        }
    }

    fn mock(ids: &[&str]) -> MockProvider {
        MockProvider::new(vec![]).with_discovered_models(ids.iter().map(|id| model(id)).collect())
    }

    fn resolver(claude: MockProvider, codex: MockProvider, vibe: MockProvider) -> Resolver {
        Arc::new(move |kind| {
            Ok(match kind {
                ProviderKind::Claude => Arc::new(claude.clone()) as Arc<dyn Provider>,
                ProviderKind::Codex => Arc::new(codex.clone()),
                ProviderKind::Vibe => Arc::new(vibe.clone()),
            })
        })
    }

    fn builder(provider: ProviderName, model: &str) -> DaemonSettings {
        DaemonSettings {
            research_provider: provider,
            planner_provider: provider,
            builder_provider: provider,
            reviewer_provider: provider,
            discovery_provider: provider,
            orchestrator_provider: provider,
            builder_model: model.into(),
            ..Default::default()
        }
    }

    const LONG: Duration = Duration::from_secs(5);

    async fn validate(settings: &DaemonSettings, resolver: &Resolver) -> Vec<String> {
        validate_models(settings, resolver, None, LONG).await
    }

    fn unknown(field: &str, model: &str, harness: &str) -> String {
        format!(
            "`{field} = {model:?}` is not among the models {harness} reports; it is passed to {harness} unchanged"
        )
    }

    #[test]
    fn base_id_strips_one_trailing_bracket_suffix() {
        assert_eq!(base_id("sonnet[1m]"), "sonnet");
        assert_eq!(base_id("opus"), "opus");
        assert_eq!(base_id("x]"), "x]");
        assert_eq!(base_id("a[b]c"), "a[b]c");
    }

    #[tokio::test]
    async fn a_known_model_gives_no_warning() {
        let claude = mock(&["opus", "sonnet"]);
        let r = resolver(claude.clone(), mock(&[]), mock(&[]));
        let settings = builder(ProviderName::Claude, "opus");
        assert!(validate(&settings, &r).await.is_empty());
        assert_eq!(claude.discovery_calls(), 1);
    }

    #[tokio::test]
    async fn bracket_suffixes_are_ignored_on_both_sides() {
        let r = resolver(mock(&["sonnet"]), mock(&[]), mock(&[]));
        let settings = builder(ProviderName::Claude, "sonnet[1m]");
        assert!(validate(&settings, &r).await.is_empty());
        let r = resolver(mock(&["sonnet[1m]"]), mock(&[]), mock(&[]));
        let settings = builder(ProviderName::Claude, "sonnet");
        assert!(validate(&settings, &r).await.is_empty());
    }

    #[tokio::test]
    async fn an_unknown_model_warns() {
        let r = resolver(mock(&["sonnet"]), mock(&[]), mock(&[]));
        let settings = builder(ProviderName::Claude, "opus");
        assert_eq!(
            validate(&settings, &r).await,
            [unknown("builder_model", "opus", "Claude Code")]
        );
    }

    #[tokio::test]
    async fn an_empty_model_is_never_checked() {
        let r = resolver(mock(&["sonnet"]), mock(&[]), mock(&[]));
        let settings = builder(ProviderName::Claude, "");
        assert!(validate(&settings, &r).await.is_empty());
    }

    #[tokio::test]
    async fn vibe_is_never_asked() {
        let vibe = mock(&[]);
        let r = resolver(mock(&[]), mock(&[]), vibe.clone());
        let settings = builder(ProviderName::Vibe, "nope");
        assert!(validate(&settings, &r).await.is_empty());
        assert_eq!(vibe.discovery_calls(), 0);
    }

    #[tokio::test]
    async fn an_unsupported_discovery_is_reported_as_not_checked() {
        let r = resolver(MockProvider::new(vec![]), mock(&[]), mock(&[]));
        let settings = builder(ProviderName::Claude, "opus");
        let warnings = validate(&settings, &r).await;
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].starts_with("Claude Code models not checked: "),
            "{warnings:?}"
        );
    }

    #[tokio::test]
    async fn a_resolver_failure_is_reported_as_not_checked() {
        let r: Resolver = Arc::new(|_| {
            Err(ProviderError::CliNotFound {
                cli: "claude".into(),
            })
        });
        let settings = builder(ProviderName::Claude, "opus");
        let warnings = validate(&settings, &r).await;
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].starts_with("Claude Code models not checked: "),
            "{warnings:?}"
        );
        assert!(warnings[0].contains("claude"), "{warnings:?}");
    }

    #[tokio::test]
    async fn a_slow_harness_times_out() {
        let slow = mock(&["opus"]).with_discovery_delay(Duration::from_secs(5));
        let r = resolver(slow, mock(&[]), mock(&[]));
        let settings = builder(ProviderName::Claude, "opus");
        let warnings = validate_models(&settings, &r, None, Duration::from_millis(50)).await;
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("timed out"), "{warnings:?}");
    }

    #[tokio::test]
    async fn providers_are_discovered_concurrently() {
        let delay = Duration::from_millis(400);
        let claude = mock(&["opus"]).with_discovery_delay(delay);
        let codex = mock(&["gpt"]).with_discovery_delay(delay);
        let r = resolver(claude.clone(), codex.clone(), mock(&[]));
        let settings = DaemonSettings {
            builder_provider: ProviderName::Claude,
            reviewer_provider: ProviderName::Codex,
            ..Default::default()
        };
        let started = Instant::now();
        validate(&settings, &r).await;
        assert!(started.elapsed() < Duration::from_millis(750));
        assert_eq!(claude.discovery_calls(), 1);
        assert_eq!(codex.discovery_calls(), 1);
    }

    #[tokio::test]
    async fn a_fresh_cache_entry_wins_over_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        cache
            .store("mock", "mock 1.0", &[model("opus")], SystemTime::now())
            .unwrap();
        let claude = mock(&["haiku"]);
        let r = resolver(claude.clone(), mock(&[]), mock(&[]));
        let settings = builder(ProviderName::Claude, "opus");
        let warnings = validate_models(&settings, &r, Some(&cache), LONG).await;
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(claude.discovery_calls(), 0);
    }

    #[tokio::test]
    async fn a_cache_for_another_version_is_refreshed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        cache
            .store("mock", "old", &[model("opus")], SystemTime::now())
            .unwrap();
        let claude = mock(&["haiku"]);
        let r = resolver(claude.clone(), mock(&[]), mock(&[]));
        let settings = builder(ProviderName::Claude, "opus");
        let warnings = validate_models(&settings, &r, Some(&cache), LONG).await;
        assert_eq!(warnings, [unknown("builder_model", "opus", "Claude Code")]);
        assert_eq!(claude.discovery_calls(), 1);
        assert_eq!(
            cache.load("mock", "mock 1.0", SystemTime::now()),
            Some(vec![model("haiku")])
        );
    }
}
