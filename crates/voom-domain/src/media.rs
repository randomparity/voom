use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::transition::FileStatus;

/// Cached fingerprint used to decide whether a previously-hashed file needs
/// to be re-hashed during discovery.
///
/// The caller (typically the CLI) supplies one of these (looked up from the
/// storage layer) for each file being scanned. Discovery compares the file's
/// on-disk `size` and `mtime` against these cached values — if the file has
/// not grown or shrunk and its `mtime` is no later than `last_seen`, the
/// stored `content_hash` is reused instead of re-reading the file.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StoredFingerprint {
    /// File size in bytes at the time the hash was computed.
    pub size: u64,
    /// Previously computed content hash.
    pub content_hash: String,
    /// Timestamp after which a newer `mtime` on disk means the content may
    /// have changed. Typically `MediaFile::introspected_at`.
    pub last_seen: DateTime<Utc>,
}

impl StoredFingerprint {
    #[must_use]
    pub fn new(size: u64, content_hash: impl Into<String>, last_seen: DateTime<Utc>) -> Self {
        Self {
            size,
            content_hash: content_hash.into(),
            last_seen,
        }
    }
}

/// A media file with full introspection metadata.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFile {
    pub id: Uuid,
    pub path: PathBuf,
    pub size: u64,
    pub content_hash: Option<String>,
    #[serde(default)]
    pub expected_hash: Option<String>,
    #[serde(default)]
    pub status: FileStatus,
    pub container: Container,
    pub duration: f64,
    pub bitrate: Option<u32>,
    pub tracks: Vec<Track>,
    pub tags: HashMap<String, String>,
    pub plugin_metadata: HashMap<String, serde_json::Value>,
    pub introspected_at: DateTime<Utc>,
}

impl MediaFile {
    /// Create a new `MediaFile` with default metadata.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use voom_domain::media::{Container, MediaFile, Track, TrackType};
    ///
    /// let file = MediaFile::new(PathBuf::from("/movies/test.mkv"))
    ///     .with_container(Container::Mkv)
    ///     .with_duration(7200.0)
    ///     .with_tracks(vec![
    ///         Track::new(0, TrackType::Video, "h264".into()),
    ///         Track::new(1, TrackType::AudioMain, "aac".into()),
    ///     ]);
    ///
    /// assert_eq!(file.container, Container::Mkv);
    /// assert_eq!(file.duration, 7200.0);
    /// assert_eq!(file.tracks.len(), 2);
    /// assert_eq!(file.video_tracks().len(), 1);
    /// assert_eq!(file.audio_tracks().len(), 1);
    /// ```
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4(),
            path,
            size: 0,
            content_hash: None,
            expected_hash: None,
            status: FileStatus::Active,
            container: Container::Other,
            duration: 0.0,
            bitrate: None,
            tracks: Vec::new(),
            tags: HashMap::new(),
            plugin_metadata: HashMap::new(),
            introspected_at: Utc::now(),
        }
    }

    #[must_use]
    pub fn tracks_of_type(&self, track_type: TrackType) -> Vec<&Track> {
        self.tracks
            .iter()
            .filter(|t| t.track_type == track_type)
            .collect()
    }

    #[must_use]
    pub fn video_tracks(&self) -> Vec<&Track> {
        self.tracks_of_type(TrackType::Video)
    }

    #[must_use]
    pub fn audio_tracks(&self) -> Vec<&Track> {
        self.tracks
            .iter()
            .filter(|t| t.track_type.is_audio())
            .collect()
    }

    #[must_use]
    pub fn subtitle_tracks(&self) -> Vec<&Track> {
        self.tracks
            .iter()
            .filter(|t| t.track_type.is_subtitle())
            .collect()
    }

    #[must_use]
    pub fn with_tracks(mut self, tracks: Vec<Track>) -> Self {
        self.tracks = tracks;
        self
    }

    #[must_use]
    pub fn with_container(mut self, container: Container) -> Self {
        self.container = container;
        self
    }

    #[must_use]
    pub fn with_duration(mut self, duration: f64) -> Self {
        self.duration = duration;
        self
    }

    #[must_use]
    pub fn with_tags(mut self, tags: HashMap<String, String>) -> Self {
        self.tags = tags;
        self
    }
}

/// A single track within a media file.
#[non_exhaustive]
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub index: u32,
    pub track_type: TrackType,
    pub codec: String,
    pub language: String,
    pub title: String,
    pub is_default: bool,
    pub is_forced: bool,
    // Audio-specific
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u32>,
    // Video-specific
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
    pub is_vfr: bool,
    pub is_hdr: bool,
    pub hdr_format: Option<String>,
    pub pixel_format: Option<String>,
}

impl Default for Track {
    fn default() -> Self {
        Self {
            index: 0,
            track_type: TrackType::Video,
            codec: String::new(),
            language: "und".to_string(),
            title: String::new(),
            is_default: false,
            is_forced: false,
            channels: None,
            channel_layout: None,
            sample_rate: None,
            bit_depth: None,
            width: None,
            height: None,
            frame_rate: None,
            is_vfr: false,
            is_hdr: false,
            hdr_format: None,
            pixel_format: None,
        }
    }
}

impl Track {
    /// Create a new track with the given index, type, and codec.
    ///
    /// # Examples
    ///
    /// ```
    /// use voom_domain::media::{Track, TrackType};
    ///
    /// let video = Track::new(0, TrackType::Video, "hevc".into());
    /// assert_eq!(video.index, 0);
    /// assert!(video.track_type.is_video());
    /// assert_eq!(video.codec, "hevc");
    /// assert_eq!(video.language, "und"); // default
    ///
    /// let audio = Track::new(1, TrackType::AudioMain, "aac".into());
    /// assert!(audio.track_type.is_audio());
    /// assert!(!audio.track_type.is_subtitle());
    /// ```
    #[must_use]
    pub fn new(index: u32, track_type: TrackType, codec: String) -> Self {
        Self {
            index,
            track_type,
            codec,
            ..Default::default()
        }
    }
}

/// The type/role of a track within a media file.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackType {
    Video,
    AudioMain,
    AudioAlternate,
    AudioCommentary,
    AudioMusic,
    AudioSfx,
    AudioNonSpeech,
    SubtitleMain,
    SubtitleForced,
    SubtitleCommentary,
    Attachment,
}

impl TrackType {
    /// Returns `true` for all audio track type variants.
    ///
    /// # Examples
    ///
    /// ```
    /// use voom_domain::media::TrackType;
    ///
    /// assert!(TrackType::AudioMain.is_audio());
    /// assert!(TrackType::AudioCommentary.is_audio());
    /// assert!(!TrackType::Video.is_audio());
    /// assert!(!TrackType::SubtitleMain.is_audio());
    /// ```
    #[must_use]
    pub fn is_audio(&self) -> bool {
        matches!(
            self,
            TrackType::AudioMain
                | TrackType::AudioAlternate
                | TrackType::AudioCommentary
                | TrackType::AudioMusic
                | TrackType::AudioSfx
                | TrackType::AudioNonSpeech
        )
    }

    #[must_use]
    pub fn is_subtitle(&self) -> bool {
        matches!(
            self,
            TrackType::SubtitleMain | TrackType::SubtitleForced | TrackType::SubtitleCommentary
        )
    }

    #[must_use]
    pub fn is_video(&self) -> bool {
        matches!(self, TrackType::Video)
    }

    /// Returns the broad category of this track type: "video", "audio", "subtitle", or "attachment".
    #[must_use]
    pub fn track_category(&self) -> &'static str {
        if self.is_video() {
            "video"
        } else if self.is_audio() {
            "audio"
        } else if self.is_subtitle() {
            "subtitle"
        } else {
            "attachment"
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            TrackType::Video => "video",
            TrackType::AudioMain => "audio_main",
            TrackType::AudioAlternate => "audio_alternate",
            TrackType::AudioCommentary => "audio_commentary",
            TrackType::AudioMusic => "audio_music",
            TrackType::AudioSfx => "audio_sfx",
            TrackType::AudioNonSpeech => "audio_non_speech",
            TrackType::SubtitleMain => "subtitle_main",
            TrackType::SubtitleForced => "subtitle_forced",
            TrackType::SubtitleCommentary => "subtitle_commentary",
            TrackType::Attachment => "attachment",
        }
    }
}

/// Parse a `TrackType` from its string representation.
///
/// # Examples
///
/// ```
/// use voom_domain::media::TrackType;
///
/// let tt: TrackType = "audio_main".parse().unwrap();
/// assert!(tt.is_audio());
///
/// let bad: Result<TrackType, _> = "not_a_type".parse();
/// assert!(bad.is_err());
/// ```
impl std::str::FromStr for TrackType {
    type Err = crate::errors::VoomError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "video" => Ok(TrackType::Video),
            "audio_main" => Ok(TrackType::AudioMain),
            "audio_alternate" => Ok(TrackType::AudioAlternate),
            "audio_commentary" => Ok(TrackType::AudioCommentary),
            "audio_music" => Ok(TrackType::AudioMusic),
            "audio_sfx" => Ok(TrackType::AudioSfx),
            "audio_non_speech" => Ok(TrackType::AudioNonSpeech),
            "subtitle_main" => Ok(TrackType::SubtitleMain),
            "subtitle_forced" => Ok(TrackType::SubtitleForced),
            "subtitle_commentary" => Ok(TrackType::SubtitleCommentary),
            "attachment" => Ok(TrackType::Attachment),
            other => Err(crate::errors::VoomError::Validation(format!(
                "unknown track type: {other}"
            ))),
        }
    }
}

/// Container format of a media file.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Container {
    Mkv,
    Mp4,
    Avi,
    Webm,
    Flv,
    Wmv,
    Mov,
    Ts,
    Other,
}

impl Container {
    /// Parse a container format from a file extension.
    ///
    /// # Examples
    ///
    /// ```
    /// use voom_domain::media::Container;
    ///
    /// assert_eq!(Container::from_extension("mkv"), Container::Mkv);
    /// assert_eq!(Container::from_extension("mp4"), Container::Mp4);
    /// assert_eq!(Container::from_extension("MKV"), Container::Mkv); // case-insensitive
    /// assert_eq!(Container::from_extension("xyz"), Container::Other);
    /// ```
    #[must_use]
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_ascii_lowercase().as_str() {
            "mkv" | "mka" | "mks" => Container::Mkv,
            "mp4" | "m4v" | "m4a" => Container::Mp4,
            "avi" => Container::Avi,
            "webm" => Container::Webm,
            "flv" => Container::Flv,
            "wmv" | "wma" => Container::Wmv,
            "mov" => Container::Mov,
            "ts" | "m2ts" | "mts" => Container::Ts,
            _ => Container::Other,
        }
    }

    /// Return every extension that `from_extension` recognises as a known container.
    ///
    /// Order is the canonical display order used in error messages. Duplicates
    /// from the same `Container` variant are preserved (e.g., `mkv`, `mka`, `mks`
    /// all map to `Mkv`).
    ///
    /// # Examples
    ///
    /// ```
    /// use voom_domain::media::Container;
    ///
    /// let exts = Container::known_extensions();
    /// assert!(exts.contains(&"mkv"));
    /// assert!(exts.contains(&"m2ts"));
    /// assert!(!exts.contains(&"xyz"));
    /// ```
    #[must_use]
    pub const fn known_extensions() -> &'static [&'static str] {
        &[
            "mkv", "mka", "mks", "mp4", "m4v", "m4a", "avi", "webm", "flv", "wmv", "wma", "mov",
            "ts", "m2ts", "mts",
        ]
    }

    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Container::Mkv => "mkv",
            Container::Mp4 => "mp4",
            Container::Avi => "avi",
            Container::Webm => "webm",
            Container::Flv => "flv",
            Container::Wmv => "wmv",
            Container::Mov => "mov",
            Container::Ts => "ts",
            Container::Other => "other",
        }
    }

    /// Map to the `FFmpeg` muxer format name used in capability announcements.
    ///
    /// Returns `None` for `Other` (unknown containers).
    #[must_use]
    pub fn ffmpeg_format_name(&self) -> Option<&'static str> {
        match self {
            Container::Mkv => Some("matroska"),
            Container::Mp4 => Some("mp4"),
            Container::Avi => Some("avi"),
            Container::Webm => Some("webm"),
            Container::Flv => Some("flv"),
            Container::Wmv => Some("asf"),
            Container::Mov => Some("mov"),
            Container::Ts => Some("mpegts"),
            Container::Other => None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn test_media_file_new() {
        let mf = MediaFile::new(PathBuf::from("/test/video.mkv"));
        assert_eq!(mf.path, PathBuf::from("/test/video.mkv"));
        assert_eq!(mf.container, Container::Other);
        assert!(mf.tracks.is_empty());
        assert_eq!(mf.expected_hash, None);
        assert_eq!(mf.status, FileStatus::Active);
    }

    #[test]
    fn test_media_file_serde_defaults_for_missing_fields() {
        // Simulate an old JSON record that does not include the new fields.
        let json = r#"{
            "id": "00000000-0000-0000-0000-000000000001",
            "path": "/test/video.mkv",
            "size": 0,
            "content_hash": null,
            "container": "Other",
            "duration": 0.0,
            "bitrate": null,
            "tracks": [],
            "tags": {},
            "plugin_metadata": {},
            "introspected_at": "2024-01-01T00:00:00Z"
        }"#;
        let mf: MediaFile = serde_json::from_str(json).expect("deserialize old record");
        assert_eq!(
            mf.expected_hash, None,
            "expected_hash should default to None"
        );
        assert_eq!(
            mf.status,
            FileStatus::Active,
            "status should default to Active"
        );
    }

    #[test]
    fn test_track_type_classification() {
        assert!(TrackType::AudioMain.is_audio());
        assert!(TrackType::AudioCommentary.is_audio());
        assert!(!TrackType::Video.is_audio());
        assert!(TrackType::SubtitleMain.is_subtitle());
        assert!(!TrackType::AudioMain.is_subtitle());
        assert!(TrackType::Video.is_video());
    }

    #[test]
    fn test_container_from_extension() {
        assert_eq!(Container::from_extension("mkv"), Container::Mkv);
        assert_eq!(Container::from_extension("MKV"), Container::Mkv);
        assert_eq!(Container::from_extension("mp4"), Container::Mp4);
        assert_eq!(Container::from_extension("m2ts"), Container::Ts);
        assert_eq!(Container::from_extension("xyz"), Container::Other);
    }

    #[test]
    fn known_extensions_matches_from_extension() {
        for ext in Container::known_extensions() {
            assert_ne!(
                Container::from_extension(ext),
                Container::Other,
                "{ext} is advertised as known but from_extension returns Other"
            );
        }
    }

    #[test]
    fn known_extensions_covers_every_non_other_container_variant() {
        let covered: Vec<Container> = Container::known_extensions()
            .iter()
            .map(|e| Container::from_extension(e))
            .collect();

        // Exhaustive match: adding a new variant to Container forces a compile
        // error here, prompting the contributor to also extend known_extensions.
        for variant in [
            Container::Mkv,
            Container::Mp4,
            Container::Avi,
            Container::Webm,
            Container::Flv,
            Container::Wmv,
            Container::Mov,
            Container::Ts,
        ] {
            assert!(
                covered.contains(&variant),
                "Container::{variant:?} has no extension in known_extensions"
            );
        }

        // Compile-time enumeration sanity check: this match is exhaustive
        // (no `_` arm), so adding a variant to Container fails compilation
        // until the array above is also extended.
        fn _exhaustive_marker(c: Container) {
            match c {
                Container::Mkv
                | Container::Mp4
                | Container::Avi
                | Container::Webm
                | Container::Flv
                | Container::Wmv
                | Container::Mov
                | Container::Ts
                | Container::Other => {}
            }
        }
    }

    #[test]
    fn test_tracks_by_type() {
        let mut mf = MediaFile::new(PathBuf::from("/test.mkv"));
        mf.tracks = vec![
            Track::new(0, TrackType::Video, "hevc".into()),
            Track::new(1, TrackType::AudioMain, "aac".into()),
            Track::new(2, TrackType::AudioCommentary, "aac".into()),
            Track::new(3, TrackType::SubtitleMain, "srt".into()),
        ];
        assert_eq!(mf.video_tracks().len(), 1);
        assert_eq!(mf.audio_tracks().len(), 2);
        assert_eq!(mf.subtitle_tracks().len(), 1);
    }

    #[test]
    fn test_container_ffmpeg_format_name() {
        assert_eq!(Container::Mkv.ffmpeg_format_name(), Some("matroska"));
        assert_eq!(Container::Mp4.ffmpeg_format_name(), Some("mp4"));
        assert_eq!(Container::Avi.ffmpeg_format_name(), Some("avi"));
        assert_eq!(Container::Webm.ffmpeg_format_name(), Some("webm"));
        assert_eq!(Container::Flv.ffmpeg_format_name(), Some("flv"));
        assert_eq!(Container::Wmv.ffmpeg_format_name(), Some("asf"));
        assert_eq!(Container::Mov.ffmpeg_format_name(), Some("mov"));
        assert_eq!(Container::Ts.ffmpeg_format_name(), Some("mpegts"));
        assert_eq!(Container::Other.ffmpeg_format_name(), None);
    }

    #[test]
    fn test_media_file_serde_json_roundtrip() {
        let mut mf = MediaFile::new(PathBuf::from("/test.mkv"));
        mf.container = Container::Mkv;
        mf.duration = 120.5;
        mf.tracks
            .push(Track::new(0, TrackType::Video, "hevc".into()));

        let json = serde_json::to_string(&mf).unwrap();
        let deserialized: MediaFile = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.path, mf.path);
        assert_eq!(deserialized.container, mf.container);
        assert_eq!(deserialized.tracks.len(), 1);
    }

    #[test]
    fn test_media_file_builder_methods() {
        let mf = MediaFile::new(PathBuf::from("/test.mkv"))
            .with_container(Container::Mkv)
            .with_duration(120.5)
            .with_tracks(vec![Track::new(0, TrackType::Video, "hevc".into())])
            .with_tags(HashMap::from([("title".into(), "Test Movie".into())]));

        assert_eq!(mf.container, Container::Mkv);
        assert_eq!(mf.duration, 120.5);
        assert_eq!(mf.tracks.len(), 1);
        assert_eq!(mf.tags["title"], "Test Movie");
    }

    #[test]
    fn test_media_file_serde_msgpack_roundtrip() {
        let mut mf = MediaFile::new(PathBuf::from("/test.mkv"));
        mf.container = Container::Mkv;
        mf.tracks
            .push(Track::new(0, TrackType::Video, "hevc".into()));

        let bytes = rmp_serde::to_vec(&mf).unwrap();
        let deserialized: MediaFile = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(deserialized.path, mf.path);
        assert_eq!(deserialized.container, mf.container);
    }
}
