//! The start-up model validation over the engine, against the mock provider: the
//! unknown-model warning, cancellation on shutdown, that spawning never blocks, and the
//! published model catalog (start-up discovery and discovery for newly assigned providers).

mod support;

use std::time::Duration;

use patok_config::SettingValue;
use patok_core::event::{EngineEvent, ModelCatalogStatus, ProviderCatalog};
use patok_providers::mock::MockProvider;
use patok_providers::{DiscoveredModel, ModelCache};
use support::{Fixture, collect_until};

const ALL_CLAUDE: &str = "research_provider = \"claude\"\nplanner_provider = \"claude\"\nbuilder_provider = \"claude\"\nreviewer_provider = \"claude\"\ndiscovery_provider = \"claude\"\norchestrator_provider = \"claude\"\nbuilder_model = \"opus\"\n";

fn catalog_events(events: &[EngineEvent]) -> Vec<ProviderCatalog> {
    events
        .iter()
        .filter_map(|event| match event {
            EngineEvent::ModelCatalogChanged { catalog } => Some(catalog.clone()),
            _ => None,
        })
        .collect()
}

fn statuses(events: &[EngineEvent], provider: &str) -> Vec<ModelCatalogStatus> {
    catalog_events(events)
        .into_iter()
        .filter(|c| c.provider == provider)
        .map(|c| c.status)
        .collect()
}

fn is_catalog(provider: &'static str, status: ModelCatalogStatus) -> impl Fn(&EngineEvent) -> bool {
    move |event| {
        matches!(event, EngineEvent::ModelCatalogChanged { catalog }
            if catalog.provider == provider && catalog.status == status)
    }
}

fn is_config(field: &'static str) -> impl Fn(&EngineEvent) -> bool {
    move |event| matches!(event, EngineEvent::ConfigChanged { field: f, .. } if f == field)
}

fn assign(engine: &patok_engine::Engine, field: &str, value: &str) {
    engine
        .apply_settings_change(field, SettingValue::Str(value.into()))
        .unwrap();
}

const CONFIG: &str = "builder_provider = \"claude\"\nbuilder_model = \"opus\"\n";

fn discovering(id: &str) -> MockProvider {
    MockProvider::new(vec![]).with_discovered_models(vec![DiscoveredModel {
        id: id.into(),
        display_name: None,
        listed: true,
    }])
}

#[tokio::test]
async fn an_unknown_configured_model_is_reported() {
    let fixture = Fixture::new("");
    let engine = fixture.engine_configured(discovering("sonnet[1m]"), CONFIG);
    let cache_dir = tempfile::tempdir().unwrap();
    let handle = engine.spawn_model_validation(Some(ModelCache::new(cache_dir.path())));
    let warnings = tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("validation finished")
        .unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("builder_model = \"opus\""),
        "{warnings:?}"
    );
}

#[tokio::test]
async fn shutdown_cancels_a_hanging_discovery() {
    let fixture = Fixture::new("");
    let mock = discovering("opus").with_discovery_delay(Duration::from_secs(60));
    let engine = fixture.engine_configured(mock, CONFIG);
    let handle = engine.spawn_model_validation(None);
    engine.shutdown_token().cancel();
    let warnings = tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("cancelled promptly")
        .unwrap();
    assert!(warnings.is_empty());
}

#[tokio::test]
async fn spawning_does_not_wait_for_discovery() {
    let fixture = Fixture::new("");
    let mock = discovering("opus").with_discovery_delay(Duration::from_secs(60));
    let engine = fixture.engine_configured(mock, CONFIG);
    let handle = engine.spawn_model_validation(None);
    assert!(!handle.is_finished());
    engine.shutdown_token().cancel();
    let _ = handle.await;
}

#[tokio::test]
async fn an_engine_with_a_fixed_provider_validates_nothing() {
    let fixture = Fixture::new("");
    let mock = discovering("sonnet");
    let engine = fixture.engine(mock.clone());
    let warnings = engine.spawn_model_validation(None).await.unwrap();
    assert!(warnings.is_empty());
    assert_eq!(mock.discovery_calls(), 0);
    assert!(engine.attach().unwrap().snapshot.model_catalog.is_empty());
}

#[tokio::test]
async fn startup_discovery_publishes_pending_then_ready() {
    let fixture = Fixture::new("");
    let mock = discovering("opus").with_discovery_delay(Duration::from_millis(200));
    let engine = fixture.engine_configured(mock.clone(), ALL_CLAUDE);
    let mut attachment = engine.attach().unwrap();
    let handle = engine.spawn_model_validation(None);

    let pending = engine.attach().unwrap().snapshot.model_catalog.clone();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].provider, "claude");
    assert_eq!(pending[0].status, ModelCatalogStatus::Pending);

    handle.await.unwrap();
    let events = collect_until(
        &mut attachment.events,
        is_catalog("claude", ModelCatalogStatus::Ready),
    )
    .await;
    assert_eq!(
        statuses(&events, "claude"),
        [ModelCatalogStatus::Pending, ModelCatalogStatus::Ready]
    );
    let ready = engine.attach().unwrap().snapshot.model_catalog.clone();
    assert_eq!(ready.len(), 1, "{ready:?}");
    assert_eq!(ready[0].status, ModelCatalogStatus::Ready);
    assert_eq!(ready[0].models.len(), 1);
    assert_eq!(ready[0].models[0].id, "opus");
    assert_eq!(mock.discovery_calls(), 1);
}

#[tokio::test]
async fn a_failed_discovery_publishes_failed() {
    let fixture = Fixture::new("");
    let engine = fixture.engine_configured(MockProvider::new(vec![]), ALL_CLAUDE);
    let mut attachment = engine.attach().unwrap();
    engine.spawn_model_validation(None).await.unwrap();
    collect_until(
        &mut attachment.events,
        is_catalog("claude", ModelCatalogStatus::Failed),
    )
    .await;
    let catalog = engine.attach().unwrap().snapshot.model_catalog.clone();
    assert_eq!(catalog.len(), 1, "{catalog:?}");
    assert_eq!(catalog[0].status, ModelCatalogStatus::Failed);
    assert!(!catalog[0].error.is_empty());
}

#[tokio::test]
async fn a_newly_assigned_provider_is_discovered() {
    let fixture = Fixture::new("");
    let mock = discovering("gpt");
    let engine = fixture.engine_configured(mock.clone(), ALL_CLAUDE);
    let mut attachment = engine.attach().unwrap();
    assign(&engine, "reviewer_provider", "codex");
    let events = collect_until(
        &mut attachment.events,
        is_catalog("codex", ModelCatalogStatus::Ready),
    )
    .await;
    assert_eq!(
        statuses(&events, "codex"),
        [ModelCatalogStatus::Pending, ModelCatalogStatus::Ready]
    );
    assert_eq!(mock.discovery_calls(), 1);
}

#[tokio::test]
async fn a_ready_provider_is_not_rediscovered() {
    let fixture = Fixture::new("");
    let mock = discovering("opus");
    let engine = fixture.engine_configured(mock.clone(), ALL_CLAUDE);
    let mut attachment = engine.attach().unwrap();
    engine.spawn_model_validation(None).await.unwrap();
    assert_eq!(mock.discovery_calls(), 1);

    assign(&engine, "reviewer_provider", "codex");
    collect_until(
        &mut attachment.events,
        is_catalog("codex", ModelCatalogStatus::Ready),
    )
    .await;
    assert_eq!(mock.discovery_calls(), 2);

    assign(&engine, "reviewer_provider", "claude");
    collect_until(&mut attachment.events, is_config("reviewer_provider")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(mock.discovery_calls(), 2);
    let mut later = vec![];
    while let Ok(event) = attachment.events.try_recv() {
        later.push(event);
    }
    assert!(statuses(&later, "claude").is_empty(), "{later:?}");
}

#[tokio::test]
async fn a_failed_provider_is_rediscovered_on_reassignment() {
    let fixture = Fixture::new("");
    let mock = MockProvider::new(vec![]);
    let engine = fixture.engine_configured(mock.clone(), ALL_CLAUDE);
    let mut attachment = engine.attach().unwrap();
    engine.spawn_model_validation(None).await.unwrap();
    assert_eq!(mock.discovery_calls(), 1);

    assign(&engine, "reviewer_provider", "codex");
    collect_until(
        &mut attachment.events,
        is_catalog("codex", ModelCatalogStatus::Failed),
    )
    .await;
    assert_eq!(mock.discovery_calls(), 2);

    assign(&engine, "reviewer_provider", "claude");
    let events = collect_until(&mut attachment.events, |event| {
        matches!(event, EngineEvent::ModelCatalogChanged { catalog }
                if catalog.provider == "claude" && catalog.status == ModelCatalogStatus::Failed)
    })
    .await;
    assert!(
        statuses(&events, "claude").contains(&ModelCatalogStatus::Pending),
        "{events:?}"
    );
    assert_eq!(mock.discovery_calls(), 3);
}

#[tokio::test]
async fn assigning_vibe_discovers_nothing() {
    let fixture = Fixture::new("");
    let mock = discovering("opus");
    let engine = fixture.engine_configured(mock.clone(), ALL_CLAUDE);
    let mut attachment = engine.attach().unwrap();
    assign(&engine, "reviewer_provider", "vibe");
    let events = collect_until(&mut attachment.events, is_config("reviewer_provider")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(catalog_events(&events).is_empty(), "{events:?}");
    assert!(attachment.events.try_recv().is_err());
    assert_eq!(mock.discovery_calls(), 0);
}
