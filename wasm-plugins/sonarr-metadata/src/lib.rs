//! Sonarr metadata enrichment plugin.
//!
//! Enriches TV episode files with metadata from a Sonarr instance via its API.
//! Uses host HTTP functions to query the Sonarr API, matching files by path
//! to series/season/episode records in the Sonarr database.
//!
//! # Host functions used
//!
//! - `http-get` — query Sonarr API endpoints
//! - `get-plugin-data` / `set-plugin-data` — cache API responses and store config
//! - `log` — structured logging
//!
//! # Configuration
//!
//! The plugin expects its config (stored via plugin data) to contain:
//! - `sonarr_url`: Base URL of the Sonarr instance (e.g., `http://localhost:8989`)
//! - `api_key`: Sonarr API key for authentication
//!
//! # Manifest
//!
//! ```toml
//! name = "sonarr-metadata"
//! version = "0.1.0"
//! description = "TV metadata enrichment via Sonarr API"
//! handles_events = ["file.introspected"]
//!
//! [[capabilities]]
//! [capabilities.EnrichMetadata]
//! source = "sonarr"
//! ```

use serde::{Deserialize, Serialize};
use voom_plugin_sdk::{deserialize_event, serialize_event, Event};

pub fn get_info() -> PluginInfoData {
    PluginInfoData {
        name: "sonarr-metadata".to_string(),
        version: "0.1.0".to_string(),
        capabilities: vec!["enrich_metadata:sonarr".to_string()],
    }
}

pub fn handles(event_type: &str) -> bool {
    event_type == "file.introspected"
}

/// Process a file.introspected event by looking up episode info from Sonarr.
pub fn on_event(
    event_type: &str,
    payload: &[u8],
    host: &dyn HostFunctions,
) -> Option<OnEventResult> {
    if event_type != "file.introspected" {
        return None;
    }

    let event = deserialize_event(payload).ok()?;
    let file = match &event {
        Event::FileIntrospected(e) => &e.file,
        _ => return None,
    };

    host.log("info", &format!("looking up Sonarr metadata for: {}", file.path.display()));

    let config = load_config(host)?;
    let episode = lookup_episode(host, &config, &file.path.to_string_lossy())?;

    let metadata = serde_json::json!({
        "source": "sonarr",
        "series_id": episode.series_id,
        "series_title": episode.series_title,
        "season_number": episode.season_number,
        "episode_number": episode.episode_number,
        "episode_title": episode.episode_title,
        "tvdb_id": episode.tvdb_id,
        "quality_profile": episode.quality_profile,
        "monitored": episode.monitored,
    });

    let enriched_event = Event::MetadataEnriched(
        voom_plugin_sdk::voom_domain::events::MetadataEnrichedEvent {
            path: file.path.clone(),
            source: "sonarr".to_string(),
            metadata,
        },
    );

    let produced_payload = serialize_event(&enriched_event).ok()?;

    Some(OnEventResult {
        plugin_name: "sonarr-metadata".to_string(),
        produced_events: vec![(enriched_event.event_type().to_string(), produced_payload)],
        data: None,
    })
}

// --- Host function abstraction ---

pub trait HostFunctions {
    fn http_get(&self, url: &str, headers: &[(String, String)]) -> Result<HttpResponse, String>;
    fn get_plugin_data(&self, key: &str) -> Option<Vec<u8>>;
    fn set_plugin_data(&self, key: &str, value: &[u8]) -> Result<(), String>;
    fn log(&self, level: &str, message: &str);
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

// --- Sonarr data types ---

#[derive(Debug, Serialize, Deserialize)]
pub struct SonarrConfig {
    pub sonarr_url: String,
    pub api_key: String,
}

/// A series record from Sonarr, with episode file info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SonarrSeries {
    pub id: u64,
    pub title: String,
    pub tvdb_id: Option<u64>,
    pub path: String,
    pub quality_profile: Option<String>,
    pub monitored: bool,
}

/// An episode file record matched from Sonarr.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SonarrEpisode {
    pub series_id: u64,
    pub series_title: String,
    pub season_number: u32,
    pub episode_number: u32,
    pub episode_title: String,
    pub tvdb_id: Option<u64>,
    pub quality_profile: Option<String>,
    pub monitored: bool,
    pub file_path: String,
}

// --- Internal helpers ---

fn load_config(host: &dyn HostFunctions) -> Option<SonarrConfig> {
    let data = host.get_plugin_data("config")?;
    serde_json::from_slice(&data).ok()
}

fn lookup_episode(
    host: &dyn HostFunctions,
    config: &SonarrConfig,
    file_path: &str,
) -> Option<SonarrEpisode> {
    // First, find which series this file belongs to.
    let series_url = format!("{}/api/v3/series", config.sonarr_url);
    let headers = vec![("X-Api-Key".to_string(), config.api_key.clone())];

    let response = host.http_get(&series_url, &headers).ok()?;
    if response.status != 200 {
        host.log("warn", &format!("Sonarr series API returned status {}", response.status));
        return None;
    }

    let all_series: Vec<SonarrSeries> = serde_json::from_slice(&response.body).ok()?;
    let series = all_series.into_iter().find(|s| file_path.starts_with(&s.path))?;

    // Then look up episode files for this series.
    let episodes_url = format!(
        "{}/api/v3/episodefile?seriesId={}",
        config.sonarr_url, series.id
    );
    let response = host.http_get(&episodes_url, &headers).ok()?;
    if response.status != 200 {
        return None;
    }

    let episode_files: Vec<SonarrEpisodeFile> =
        serde_json::from_slice(&response.body).ok()?;
    let matched = episode_files.into_iter().find(|ef| file_path.ends_with(&ef.relative_path))?;

    Some(SonarrEpisode {
        series_id: series.id,
        series_title: series.title.clone(),
        season_number: matched.season_number,
        episode_number: matched.episode_number,
        episode_title: matched.episode_title,
        tvdb_id: series.tvdb_id,
        quality_profile: series.quality_profile.clone(),
        monitored: series.monitored,
        file_path: file_path.to_string(),
    })
}

/// Internal: Sonarr episode file record from the API.
#[derive(Debug, Serialize, Deserialize)]
struct SonarrEpisodeFile {
    relative_path: String,
    season_number: u32,
    episode_number: u32,
    episode_title: String,
}

// --- Common types ---

pub struct PluginInfoData {
    pub name: String,
    pub version: String,
    pub capabilities: Vec<String>,
}

pub struct OnEventResult {
    pub plugin_name: String,
    pub produced_events: Vec<(String, Vec<u8>)>,
    pub data: Option<Vec<u8>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_plugin_sdk::*;

    struct MockHost {
        config: Option<SonarrConfig>,
        series: Vec<SonarrSeries>,
        episode_files: Vec<SonarrEpisodeFile>,
    }

    impl MockHost {
        fn new() -> Self {
            Self {
                config: Some(SonarrConfig {
                    sonarr_url: "http://localhost:8989".to_string(),
                    api_key: "test-key".to_string(),
                }),
                series: vec![SonarrSeries {
                    id: 10,
                    title: "Breaking Bad".to_string(),
                    tvdb_id: Some(81189),
                    path: "/media/tv/Breaking Bad".to_string(),
                    quality_profile: Some("HD-1080p".to_string()),
                    monitored: true,
                }],
                episode_files: vec![SonarrEpisodeFile {
                    relative_path: "Season 01/Breaking.Bad.S01E01.1080p.mkv".to_string(),
                    season_number: 1,
                    episode_number: 1,
                    episode_title: "Pilot".to_string(),
                }],
            }
        }

        fn without_config() -> Self {
            Self {
                config: None,
                series: vec![],
                episode_files: vec![],
            }
        }
    }

    impl HostFunctions for MockHost {
        fn http_get(&self, url: &str, _headers: &[(String, String)]) -> Result<HttpResponse, String> {
            let body = if url.contains("/episodefile") {
                serde_json::to_vec(&self.episode_files).unwrap()
            } else {
                serde_json::to_vec(&self.series).unwrap()
            };
            Ok(HttpResponse { status: 200, body })
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
        file
    }

    #[test]
    fn test_get_info() {
        let info = get_info();
        assert_eq!(info.name, "sonarr-metadata");
        assert_eq!(info.capabilities, vec!["enrich_metadata:sonarr"]);
    }

    #[test]
    fn test_handles() {
        assert!(handles("file.introspected"));
        assert!(!handles("file.discovered"));
    }

    #[test]
    fn test_on_event_episode_found() {
        let host = MockHost::new();
        let file = make_test_file(
            "/media/tv/Breaking Bad/Season 01/Breaking.Bad.S01E01.1080p.mkv",
        );
        let event = Event::FileIntrospected(
            voom_plugin_sdk::voom_domain::events::FileIntrospectedEvent { file },
        );
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.plugin_name, "sonarr-metadata");

        let produced: Event = deserialize_event(&result.produced_events[0].1).unwrap();
        match produced {
            Event::MetadataEnriched(e) => {
                assert_eq!(e.source, "sonarr");
                assert_eq!(e.metadata["series_title"], "Breaking Bad");
                assert_eq!(e.metadata["season_number"], 1);
                assert_eq!(e.metadata["episode_number"], 1);
                assert_eq!(e.metadata["episode_title"], "Pilot");
                assert_eq!(e.metadata["tvdb_id"], 81189);
            }
            _ => panic!("expected MetadataEnriched"),
        }
    }

    #[test]
    fn test_on_event_series_not_found() {
        let host = MockHost::new();
        let file = make_test_file("/media/tv/Unknown Show/S01E01.mkv");
        let event = Event::FileIntrospected(
            voom_plugin_sdk::voom_domain::events::FileIntrospectedEvent { file },
        );
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_no_config() {
        let host = MockHost::without_config();
        let file = make_test_file("/media/tv/test.mkv");
        let event = Event::FileIntrospected(
            voom_plugin_sdk::voom_domain::events::FileIntrospectedEvent { file },
        );
        let payload = serialize_event(&event).unwrap();

        let result = on_event("file.introspected", &payload, &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_on_event_wrong_type() {
        let host = MockHost::new();
        let result = on_event("file.discovered", &[], &host);
        assert!(result.is_none());
    }

    #[test]
    fn test_sonarr_config_serde() {
        let config = SonarrConfig {
            sonarr_url: "http://localhost:8989".to_string(),
            api_key: "key123".to_string(),
        };
        let bytes = serde_json::to_vec(&config).unwrap();
        let restored: SonarrConfig = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.sonarr_url, "http://localhost:8989");
    }
}
