//! Discovered models and their on-disk per-provider cache.
//!
//! The cache exists so that daemon starts usually spawn no harness. It is keyed by the
//! harness version, because an upgraded harness may report different models. It stores
//! only the parsed models, never the raw harness output: Claude's `initialize` response
//! carries account data, and Codex's payload is hundreds of KB.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use patok_core::utils::atomic_write::write_atomic;
use serde::{Deserialize, Serialize};

/// One model a harness reports as available.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredModel {
    /// The id passed to the harness's model flag.
    pub id: String,
    pub display_name: Option<String>,
    /// Whether the harness lists the model to users (Codex hides some).
    pub listed: bool,
}

/// How long a cache entry stays fresh.
pub const MODEL_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Serialize, Deserialize)]
struct CacheFile {
    /// Unix seconds.
    fetched_at: u64,
    harness_version: String,
    models: Vec<DiscoveredModel>,
}

/// Why a cache lookup did not yield models.
#[derive(Debug)]
enum Miss {
    Missing,
    Unreadable(std::io::Error),
    Corrupt(serde_json::Error),
    VersionMismatch { stored: String },
    Stale { age: Duration },
    FromTheFuture,
}

/// One JSON file per provider: `<dir>/<slug>.json`.
#[derive(Clone, Debug)]
pub struct ModelCache {
    dir: PathBuf,
}

impl ModelCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The cache under `<user cache dir>/patok/models`; `None` when neither
    /// `XDG_CACHE_HOME` nor `HOME` resolves.
    pub fn from_env() -> Option<Self> {
        Some(Self::new(
            patok_core::paths::cache_dir_from_env()?.join("models"),
        ))
    }

    pub fn path(&self, slug: &str) -> PathBuf {
        self.dir.join(format!("{slug}.json"))
    }

    /// The cached models of `slug` when fresh at `now` and recorded for
    /// `harness_version` (the caller's trimmed `--version` line, compared
    /// exactly); `None` when missing, unreadable, corrupt, stale or recorded
    /// for another version. Every miss logs its reason at debug level.
    pub fn load(
        &self,
        slug: &str,
        harness_version: &str,
        now: SystemTime,
    ) -> Option<Vec<DiscoveredModel>> {
        let path = self.path(slug);
        let path = path.display();
        match self.lookup(slug, harness_version, now) {
            Ok(models) => {
                tracing::debug!(slug, harness_version, models = models.len(), %path, "model cache hit");
                Some(models)
            }
            Err(Miss::Missing) => {
                tracing::debug!(slug, %path, "model cache miss: no cache file");
                None
            }
            Err(Miss::Unreadable(error)) => {
                tracing::debug!(slug, %path, %error, "model cache miss: unreadable");
                None
            }
            Err(Miss::Corrupt(error)) => {
                tracing::debug!(slug, %path, %error, "model cache miss: corrupt");
                None
            }
            Err(Miss::VersionMismatch { stored }) => {
                tracing::debug!(slug, %path, %stored, observed = harness_version, "model cache miss: harness version mismatch");
                None
            }
            Err(Miss::Stale { age }) => {
                tracing::debug!(
                    slug,
                    %path,
                    age_secs = age.as_secs(),
                    ttl_secs = MODEL_CACHE_TTL.as_secs(),
                    "model cache miss: stale"
                );
                None
            }
            Err(Miss::FromTheFuture) => {
                tracing::debug!(slug, %path, "model cache miss: fetched in the future");
                None
            }
        }
    }

    fn lookup(
        &self,
        slug: &str,
        harness_version: &str,
        now: SystemTime,
    ) -> Result<Vec<DiscoveredModel>, Miss> {
        let text = std::fs::read_to_string(self.path(slug)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Miss::Missing
            } else {
                Miss::Unreadable(error)
            }
        })?;
        let file: CacheFile = serde_json::from_str(&text).map_err(Miss::Corrupt)?;
        if file.harness_version != harness_version {
            return Err(Miss::VersionMismatch {
                stored: file.harness_version,
            });
        }
        let fetched = UNIX_EPOCH + Duration::from_secs(file.fetched_at);
        match now.duration_since(fetched) {
            Ok(age) if age < MODEL_CACHE_TTL => Ok(file.models),
            Ok(age) => Err(Miss::Stale { age }),
            Err(_) => Err(Miss::FromTheFuture),
        }
    }

    /// Atomically replaces the cache of `slug`.
    pub fn store(
        &self,
        slug: &str,
        harness_version: &str,
        models: &[DiscoveredModel],
        now: SystemTime,
    ) -> std::io::Result<()> {
        let file = CacheFile {
            fetched_at: now
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
            harness_version: harness_version.to_owned(),
            models: models.to_vec(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
        write_atomic(&self.path(slug), &json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn sample() -> Vec<DiscoveredModel> {
        vec![
            DiscoveredModel {
                id: "alpha".into(),
                display_name: Some("Alpha".into()),
                listed: true,
            },
            DiscoveredModel {
                id: "beta".into(),
                display_name: None,
                listed: false,
            },
        ]
    }

    #[test]
    fn a_stored_cache_loads_back_while_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        cache.store("claude", "2.1.0", &sample(), t0()).unwrap();
        let later = t0() + Duration::from_secs(23 * 3600);
        assert_eq!(cache.load("claude", "2.1.0", later), Some(sample()));
    }

    #[test]
    fn a_cache_older_than_a_day_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        cache.store("claude", "1", &sample(), t0()).unwrap();
        let almost = t0() + MODEL_CACHE_TTL - Duration::from_secs(1);
        assert!(cache.load("claude", "1", almost).is_some());
        assert_eq!(cache.load("claude", "1", t0() + MODEL_CACHE_TTL), None);
    }

    #[test]
    fn a_cache_from_the_future_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        cache.store("claude", "1", &sample(), t0()).unwrap();
        let earlier = t0() - Duration::from_secs(3600);
        assert_eq!(cache.load("claude", "1", earlier), None);
    }

    #[test]
    fn a_different_harness_version_invalidates_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        cache.store("claude", "2.1.0", &sample(), t0()).unwrap();
        assert_eq!(cache.load("claude", "2.2.0", t0()), None);
    }

    #[test]
    fn a_missing_cache_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        assert_eq!(cache.load("claude", "1", t0()), None);
    }

    #[test]
    fn a_corrupt_cache_is_treated_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        std::fs::write(cache.path("claude"), "{not json").unwrap();
        assert_eq!(cache.load("claude", "1", t0()), None);
        std::fs::write(cache.path("claude"), r#"{"models":3}"#).unwrap();
        assert_eq!(cache.load("claude", "1", t0()), None);
        cache.store("claude", "1", &sample(), t0()).unwrap();
        assert_eq!(cache.load("claude", "1", t0()), Some(sample()));
    }

    #[test]
    fn lookup_names_why_it_missed() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        assert!(matches!(
            cache.lookup("claude", "1", t0()),
            Err(Miss::Missing)
        ));
        cache.store("claude", "1", &sample(), t0()).unwrap();
        match cache.lookup("claude", "2", t0()) {
            Err(Miss::VersionMismatch { stored }) => assert_eq!(stored, "1"),
            other => panic!("unexpected: {other:?}"),
        }
        let later = t0() + MODEL_CACHE_TTL + Duration::from_secs(5);
        match cache.lookup("claude", "1", later) {
            Err(Miss::Stale { age }) => assert_eq!(age, MODEL_CACHE_TTL + Duration::from_secs(5)),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(
            cache.lookup("claude", "1", t0() - Duration::from_secs(1)),
            Err(Miss::FromTheFuture)
        ));
        std::fs::write(cache.path("claude"), "{not json").unwrap();
        assert!(matches!(
            cache.lookup("claude", "1", t0()),
            Err(Miss::Corrupt(_))
        ));
        std::fs::remove_file(cache.path("claude")).unwrap();
        std::fs::create_dir(cache.path("claude")).unwrap();
        assert!(matches!(
            cache.lookup("claude", "1", t0()),
            Err(Miss::Unreadable(_))
        ));
    }

    #[test]
    fn a_cache_stored_under_the_version_line_loads_back() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        cache
            .store("codex", "codex-cli 0.160.0", &sample(), t0())
            .unwrap();
        assert_eq!(
            cache.load("codex", "codex-cli 0.160.0", t0() + Duration::from_secs(60)),
            Some(sample())
        );
    }

    #[test]
    fn store_creates_the_directory_and_replaces_atomically() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a").join("models");
        let cache = ModelCache::new(&dir);
        cache.store("claude", "1", &sample(), t0()).unwrap();
        let second = vec![sample().remove(0)];
        cache.store("claude", "1", &second, t0()).unwrap();
        assert_eq!(cache.load("claude", "1", t0()), Some(second));
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["claude.json".to_string()]);
    }

    #[test]
    fn the_cache_file_is_one_json_per_provider() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ModelCache::new(dir.path());
        assert!(cache.path("codex").ends_with("codex.json"));
        cache.store("claude", "1", &sample(), t0()).unwrap();
        assert!(!cache.path("codex").exists());
        assert_eq!(cache.load("codex", "1", t0()), None);
    }
}
