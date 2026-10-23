//! The start-up model validation over the engine, against the mock provider: the
//! unknown-model warning, cancellation on shutdown, and that spawning never blocks.

mod support;

use std::time::Duration;

use patok_providers::mock::MockProvider;
use patok_providers::{DiscoveredModel, ModelCache};
use support::Fixture;

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
}
