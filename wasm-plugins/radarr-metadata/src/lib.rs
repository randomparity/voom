//! Radarr metadata enrichment plugin.
//!
//! Enriches movie files with metadata from a Radarr instance via its API.
//! Uses host HTTP functions to query the Radarr API, matching files by path
//! or filename to movie records in the Radarr database.
//!
//! # Host functions used
//!
//! - `http-get` — query Radarr API endpoints
//! - `get-plugin-data` / `set-plugin-data` — cache API responses
//! - `log` — structured logging
//!
//! # Configuration
//!
//! The plugin expects its config (stored via plugin data) to contain:
//! - `radarr_url`: Base URL of the Radarr instance (e.g., `http://localhost:7878`)
//! - `api_key`: Radarr API key for authentication
//!
//! # Manifest
//!
//! ```toml
//! name = "radarr-metadata"
//! version = "0.1.0"
//! description = "Movie metadata enrichment via Radarr API"
//! handles_events = ["file.introspected"]
//!
//! [[capabilities]]
//! [capabilities.EnrichMetadata]
//! source = "radarr"
//! ```

use serde::{Deserialize, Serialize};
use voom_plugin_sdk::{
    deserialize_event_or_log, language_code_from_name, load_plugin_config, serialize_event_or_log,
    Capability, Event, HostFunctions, MetadataEnrichedEvent, OnEventResult, PluginInfoData,
};

pub fn get_info() -> PluginInfoData {
    PluginInfoData::new(
        "radarr-metadata",
        "0.1.0",
        vec![Capability::EnrichMetadata {
            source: "radarr".to_string(),
        }],
    )
    .with_description("Movie metadata enrichment via Radarr API")
    .with_author("David Christensen")
    .with_license("MIT")
    .with_homepage("https://github.com/randomparity/voom")
}

pub fn handles(event_type: &str) -> bool {
    event_type == "file.introspected"
}

/// Process a file.introspected event by looking up movie info from Radarr.
///
/// In a real WASM plugin, the `http_get` function would be provided by the host
/// via WIT imports. Here we simulate the flow with a `HostFunctions` trait.
pub fn on_event(
    event_type: &str,
    payload: &[u8],
    host: &dyn HostFunctions,
) -> Option<OnEventResult> {
    if event_type != "file.introspected" {
        return None;
    }

    let event = deserialize_event_or_log(payload, host)?;
    let file = match &event {
        Event::FileIntrospected(e) => &e.file,
        _ => return None,
    };

    host.log(
        "info",
        &format!("looking up Radarr metadata for: {}", file.path.display()),
    );

    let config: RadarrConfig = match load_plugin_config(|key| host.get_plugin_data(key)) {
        Ok(Some(config)) => config,
        Ok(None) => {
            host.log("warn", "missing Radarr plugin config");
            return None;
        }
        Err(e) => {
            host.log("error", &format!("failed to load plugin config: {e}"));
            return None;
        }
    };
    let movie = lookup_movie(host, &config, &file.path.to_string_lossy())?;

    let original_language = movie
        .original_language
        .as_ref()
        .and_then(|lang| language_code_from_name(&lang.name));

    let mut metadata = serde_json::json!({
        "source": "radarr",
        "radarr_id": movie.id,
        "title": movie.title,
        "year": movie.year,
        "tmdb_id": movie.tmdb_id,
        "imdb_id": movie.imdb_id,
        "quality_profile": movie.quality_profile,
        "monitored": movie.monitored,
    });
    if let Some(lang) = original_language {
        metadata["original_language"] = serde_json::Value::String(lang.to_string());
    }

    let enriched_event = Event::MetadataEnriched(MetadataEnrichedEvent::new(
        file.path.clone(),
        "radarr".to_string(),
        metadata,
    ));

    let produced_payload = serialize_event_or_log(&enriched_event, host)?;

    Some(OnEventResult::new(
        "radarr-metadata",
        vec![(enriched_event.event_type().to_string(), produced_payload)],
        None,
    ))
}

// --- Radarr data types ---

/// Plugin configuration loaded from plugin data storage.
#[derive(Debug, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadarrConfig {
    pub radarr_url: String,
    pub api_key: String,
}

/// A movie record from the Radarr API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadarrMovie {
    pub id: u64,
    pub title: String,
    pub year: u32,
    pub tmdb_id: Option<u64>,
    pub imdb_id: Option<String>,
    pub quality_profile: Option<String>,
    pub monitored: bool,
    pub path: String,
    #[serde(default)]
    pub original_language: Option<RadarrLanguage>,
}

/// Language record from the Radarr API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RadarrLanguage {
    pub name: String,
}

// --- Internal helpers ---

fn lookup_movie(
    host: &dyn HostFunctions,
    config: &RadarrConfig,
    file_path: &str,
) -> Option<RadarrMovie> {
    let url = format!("{}/api/v3/movie", config.radarr_url);
    let headers = vec![("X-Api-Key".to_string(), config.api_key.clone())];

    let response = host
        .http_get(&url, &headers)
        .map_err(|e| {
            host.log("error", &format!("Radarr API request failed: {e}"));
        })
        .ok()?;
    if response.status != 200 {
        host.log(
            "warn",
            &format!("Radarr API returned status {}", response.status),
        );
        return None;
    }

    let movies: Vec<RadarrMovie> = serde_json::from_slice(&response.body)
        .map_err(|e| {
            host.log(
                "error",
                &format!("failed to parse Radarr API response: {e}"),
            );
        })
        .ok()?;

    // Match by file path — Radarr stores the movie's root path, and the file
    // should be under that directory.
    movies.into_iter().find(|m| file_path.starts_with(&m.path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_plugin_sdk::*;

    const TEST_RADARR_TOKEN: &str = "test-api-key";

    /// Mock host that simulates Radarr API responses.
    struct MockHost {
        config: Option<RadarrConfig>,
        movies: Vec<RadarrMovie>,
    }

    impl MockHost {
        fn new() -> Self {
            Self {
                config: Some(RadarrConfig {
                    radarr_url: "http://localhost:7878".to_string(),
                    api_key: TEST_RADARR_TOKEN.to_string(),
                }),
                movies: vec![RadarrMovie {
                    id: 42,
                    title: "Blade Runner 2049".to_string(),
                    year: 2017,
                    tmdb_id: Some(335984),
                    imdb_id: Some("tt1856101".to_string()),
                    quality_profile: Some("HD-1080p".to_string()),
                    monitored: true,
                    path: "/media/movies/Blade Runner 2049 (2017)".to_string(),
                    original_language: Some(RadarrLanguage {
                        name: "English".to_string(),
                    }),
                }],
            }
        }

        fn without_config() -> Self {
            Self {
                config: None,
                movies: vec![],
            }
        }
    }

    impl HostFunctions for MockHost {
        fn http_get(
            &self,
            _url: &str,
            _headers: &[(String, String)],
        ) -> Result<HttpResponse, String> {
            let body = serde_json::to_vec(&self.movies).unwrap();
            Ok(HttpResponse::new(200, body))
        }

        fn get_plugin_data(&self, key: &str) -> Option<Vec<u8>> {
            if key == "config" {
                self.config.as_ref().map(|c| serde_json::to_vec(c).unwrap())
            } else {
                None
            }
        }

        fn set_plugin_data(&self, _key: &str, _value: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn log(&self, _level: &str, _message: &str) {}
    }

    fn make_test_file(path: &str) -> MediaFile {
        let mut file = MediaFile::new(PathBuf::from(path));
        file.container = Container::Mkv;
        file.duration = 9780.0;
        file
    }

    #[test]
    fn test_get_info() {
        let info = get_info();
        assert_eq!(info.name, "radarr-metadata");
        assert_eq!(info.capabilities.len(), 1);
        assert_eq!(info.capabilities[0].kind(), "enrich_metadata");
    }

    #[test]
    fn test_handles() {
        assert!(handles("file.introspected"));
        assert!(!handles("file.discovered"));
        assert!(!handles("metadata.enriched"));
    }

    #[test]
    fn test_on_event_movie_found() {
        let host = MockHost::new();
        let file = make_test_file(
            "/media/movies/Blade Runner 2049 (2017)/Blade.Runner.2049.2017.1080p.mkv",
        );
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "radarr-metadata");
        assert_eq!(result.produced_events.len(), 1);

        let produced: Event = deserialize_event(&result.produced_events[0].1).unwrap();
        match produced {
            Event::MetadataEnriched(e) => {
                assert_eq!(e.source, "radarr");
                assert_eq!(e.metadata["title"], "Blade Runner 2049");
                assert_eq!(e.metadata["year"], 2017);
                assert_eq!(e.metadata["radarr_id"], 42);
                assert_eq!(e.metadata["tmdb_id"], 335984);
                assert_eq!(e.metadata["original_language"], "eng");
            }
            _ => panic!("expected MetadataEnriched"),
        }
    }

    #[test]
    fn test_on_event_movie_not_found() {
        let host = MockHost::new();
        let file = make_test_file("/media/movies/Unknown Movie/file.mkv");
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_no_config() {
        let host = MockHost::without_config();
        let file = make_test_file("/media/movies/test.mkv");
        let event = Event::FileIntrospected(FileIntrospectedEvent::new(file));
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_wrong_event_type() {
        let host = MockHost::new();
        let result = on_event("file.discovered", &[], &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_invalid_payload() {
        let host = MockHost::new();
        let result = on_event("file.introspected", &[0xFF], &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_radarr_config_serde() {
        let config = RadarrConfig {
            radarr_url: "http://localhost:7878".to_string(),
            api_key: "abc123".to_string(),
        };
        let bytes = serde_json::to_vec(&config).unwrap();
        let restored: RadarrConfig = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.radarr_url, "http://localhost:7878");
        assert_eq!(restored.api_key, "abc123");
    }
}
