//! Automatic black-bar detection using FFmpeg's `cropdetect` filter.

use std::path::Path;
use std::time::Duration;

use chrono::Utc;
use voom_domain::errors::{Result, VoomError};
use voom_domain::media::{CropDetection, CropRect};
use voom_domain::plan::CropSettings;

const DEFAULT_SAMPLE_DURATION_SECS: u32 = 60;
const DEFAULT_SAMPLE_COUNT: u32 = 3;
const DEFAULT_THRESHOLD: u8 = 24;
const DEFAULT_MINIMUM_CROP: u32 = 4;
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Source dimensions and duration needed to convert `cropdetect` output into edge crops.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropDetectSource {
    pub width: u32,
    pub height: u32,
    pub duration_secs: f64,
}

impl CropDetectSource {
    #[must_use]
    pub fn new(width: u32, height: u32, duration_secs: f64) -> Self {
        Self {
            width,
            height,
            duration_secs,
        }
    }
}

/// Run FFmpeg crop detection and return a cached crop value when useful.
///
/// # Errors
/// Returns `VoomError::ToolExecution` if `ffmpeg` fails or emits invalid crop values.
pub fn detect_crop(
    ffmpeg_path: &str,
    source_path: &Path,
    source: CropDetectSource,
    settings: &CropSettings,
) -> Result<Option<CropDetection>> {
    let config = CropDetectConfig::from(settings);
    let rects = detect_sample_rects(ffmpeg_path, source_path, source, &config)?;
    let Some(rect) = conservative_crop(&rects, source, &config)? else {
        return Ok(None);
    };
    if rect.is_empty() {
        return Ok(None);
    }
    Ok(Some(CropDetection::new(rect, Utc::now())))
}

fn detect_sample_rects(
    ffmpeg_path: &str,
    source_path: &Path,
    source: CropDetectSource,
    config: &CropDetectConfig,
) -> Result<Vec<CropRect>> {
    let mut rects = Vec::new();
    for start in sample_starts(
        source.duration_secs,
        config.sample_count,
        config.sample_duration_secs,
    ) {
        let stderr = run_cropdetect_sample(ffmpeg_path, source_path, start, config)?;
        rects.extend(parse_cropdetect_rects(&stderr, source)?);
    }
    Ok(rects)
}

fn run_cropdetect_sample(
    ffmpeg_path: &str,
    source_path: &Path,
    start_secs: f64,
    config: &CropDetectConfig,
) -> Result<String> {
    let duration = config.sample_duration_secs.to_string();
    let start = format!("{start_secs:.3}");
    let filter = format!("cropdetect=limit={}:round=2", config.threshold);
    let args = [
        "-hide_banner",
        "-ss",
        &start,
        "-t",
        &duration,
        "-i",
        &source_path.to_string_lossy(),
        "-vf",
        &filter,
        "-f",
        "null",
        "-",
    ];
    let output = voom_process::run_with_timeout(
        ffmpeg_path,
        &args,
        Duration::from_secs(DEFAULT_TIMEOUT_SECS),
    )?;
    if !output.status.success() {
        return Err(VoomError::ToolExecution {
            tool: ffmpeg_path.to_string(),
            message: format!(
                "cropdetect failed with {}:\n{}",
                output.status,
                voom_process::stderr_tail(&output.stderr, 20)
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stderr).to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CropDetectConfig {
    sample_duration_secs: u32,
    sample_count: u32,
    threshold: u8,
    minimum_crop: u32,
    preserve_bottom_pixels: u32,
    aspect_locks: Vec<AspectRatio>,
}

impl From<&CropSettings> for CropDetectConfig {
    fn from(settings: &CropSettings) -> Self {
        Self {
            sample_duration_secs: settings
                .sample_duration_secs
                .unwrap_or(DEFAULT_SAMPLE_DURATION_SECS)
                .max(1),
            sample_count: settings.sample_count.unwrap_or(DEFAULT_SAMPLE_COUNT).max(1),
            threshold: settings.threshold.unwrap_or(DEFAULT_THRESHOLD),
            minimum_crop: settings.minimum_crop.unwrap_or(DEFAULT_MINIMUM_CROP),
            preserve_bottom_pixels: settings.preserve_bottom_pixels.unwrap_or(0),
            aspect_locks: settings
                .aspect_lock
                .iter()
                .filter_map(|value| AspectRatio::parse(value))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AspectRatio {
    width: u32,
    height: u32,
}

impl AspectRatio {
    fn parse(value: &str) -> Option<Self> {
        let (width, height) = value.split_once('/')?;
        let width = width.parse().ok()?;
        let height = height.parse().ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        Some(Self { width, height })
    }
}

fn sample_starts(duration_secs: f64, count: u32, sample_duration_secs: u32) -> Vec<f64> {
    if duration_secs <= 0.0 || count == 1 {
        return vec![0.0];
    }
    let latest_start = (duration_secs - f64::from(sample_duration_secs)).max(0.0);
    if latest_start == 0.0 {
        return vec![0.0];
    }
    (0..count)
        .map(|idx| latest_start * f64::from(idx) / f64::from(count - 1))
        .collect()
}

fn parse_cropdetect_rects(stderr: &str, source: CropDetectSource) -> Result<Vec<CropRect>> {
    stderr
        .lines()
        .filter_map(parse_crop_line)
        .map(|crop| crop.to_rect(source))
        .collect()
}

fn parse_crop_line(line: &str) -> Option<DetectedCrop> {
    let crop = line.split("crop=").last()?;
    let mut parts = crop.split(|c: char| c == ':' || c.is_whitespace());
    Some(DetectedCrop {
        width: parts.next()?.parse().ok()?,
        height: parts.next()?.parse().ok()?,
        x: parts.next()?.parse().ok()?,
        y: parts.next()?.parse().ok()?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DetectedCrop {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
}

impl DetectedCrop {
    fn to_rect(self, source: CropDetectSource) -> Result<CropRect> {
        let right = source
            .width
            .checked_sub(self.x)
            .and_then(|v| v.checked_sub(self.width))
            .ok_or_else(|| invalid_crop("crop width exceeds source width"))?;
        let bottom = source
            .height
            .checked_sub(self.y)
            .and_then(|v| v.checked_sub(self.height))
            .ok_or_else(|| invalid_crop("crop height exceeds source height"))?;
        Ok(CropRect::new(self.x, self.y, right, bottom))
    }
}

fn conservative_crop(
    rects: &[CropRect],
    source: CropDetectSource,
    config: &CropDetectConfig,
) -> Result<Option<CropRect>> {
    let Some(first) = rects.first().copied() else {
        return Ok(None);
    };
    let mut rect = rects.iter().skip(1).fold(first, |acc, item| {
        CropRect::new(
            acc.left.min(item.left),
            acc.top.min(item.top),
            acc.right.min(item.right),
            acc.bottom.min(item.bottom),
        )
    });
    rect = apply_minimum_crop(rect, config.minimum_crop);
    rect.bottom = rect.bottom.saturating_sub(config.preserve_bottom_pixels);
    rect = snap_to_aspect_lock(rect, source, &config.aspect_locks)?;
    rect = snap_output_to_even(rect, source)?;
    Ok(Some(rect))
}

fn apply_minimum_crop(rect: CropRect, minimum: u32) -> CropRect {
    let side = |value| if value < minimum { 0 } else { value };
    CropRect::new(
        side(rect.left),
        side(rect.top),
        side(rect.right),
        side(rect.bottom),
    )
}

fn snap_to_aspect_lock(
    rect: CropRect,
    source: CropDetectSource,
    aspect_locks: &[AspectRatio],
) -> Result<CropRect> {
    if aspect_locks.is_empty() {
        return Ok(rect);
    }
    let original_dims = output_dimensions(rect, source)?;
    let mut best: Option<(CropRect, u64)> = None;
    for ratio in aspect_locks {
        let Some((candidate, candidate_dims)) = snap_to_aspect(rect, source, original_dims, *ratio)
        else {
            continue;
        };
        let expanded_pixels = candidate_dims.area().saturating_sub(original_dims.area());
        if best
            .as_ref()
            .is_none_or(|(_, best_pixels)| expanded_pixels < *best_pixels)
        {
            best = Some((candidate, expanded_pixels));
        }
    }
    Ok(best.map_or(rect, |(candidate, _)| candidate))
}

fn snap_to_aspect(
    rect: CropRect,
    source: CropDetectSource,
    dims: OutputDimensions,
    ratio: AspectRatio,
) -> Option<(CropRect, OutputDimensions)> {
    let current = u64::from(dims.width) * u64::from(ratio.height);
    let target = u64::from(dims.height) * u64::from(ratio.width);
    if current == target {
        return Some((rect, dims));
    }
    if current > target {
        let height = even_u64_to_u32(ceil_div_u64(
            u64::from(dims.width) * u64::from(ratio.height),
            u64::from(ratio.width),
        ))?;
        if height > source.height {
            return None;
        }
        let (top, bottom) = expand_axis(rect.top, rect.bottom, height - dims.height)?;
        return Some((
            CropRect::new(rect.left, top, rect.right, bottom),
            OutputDimensions {
                width: dims.width,
                height,
            },
        ));
    }

    let width = even_u64_to_u32(ceil_div_u64(
        u64::from(dims.height) * u64::from(ratio.width),
        u64::from(ratio.height),
    ))?;
    if width > source.width {
        return None;
    }
    let (left, right) = expand_axis(rect.left, rect.right, width - dims.width)?;
    Some((
        CropRect::new(left, rect.top, right, rect.bottom),
        OutputDimensions {
            width,
            height: dims.height,
        },
    ))
}

#[derive(Debug, Clone, Copy)]
struct OutputDimensions {
    width: u32,
    height: u32,
}

impl OutputDimensions {
    fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

fn output_dimensions(rect: CropRect, source: CropDetectSource) -> Result<OutputDimensions> {
    let width = source
        .width
        .checked_sub(rect.left)
        .and_then(|v| v.checked_sub(rect.right))
        .ok_or_else(|| invalid_crop("crop removes all source width"))?;
    let height = source
        .height
        .checked_sub(rect.top)
        .and_then(|v| v.checked_sub(rect.bottom))
        .ok_or_else(|| invalid_crop("crop removes all source height"))?;
    if width == 0 || height == 0 {
        return Err(invalid_crop("crop leaves empty output"));
    }
    Ok(OutputDimensions { width, height })
}

fn ceil_div_u64(numerator: u64, denominator: u64) -> u64 {
    numerator.div_ceil(denominator)
}

fn even_u64_to_u32(value: u64) -> Option<u32> {
    let value = value.checked_add(value % 2)?;
    u32::try_from(value).ok()
}

fn expand_axis(start_crop: u32, end_crop: u32, expand_by: u32) -> Option<(u32, u32)> {
    if expand_by > start_crop.saturating_add(end_crop) {
        return None;
    }
    let from_start = (expand_by / 2).min(start_crop);
    let remaining = expand_by - from_start;
    let from_end = remaining.min(end_crop);
    let remaining = remaining - from_end;
    if remaining > start_crop - from_start {
        return None;
    }
    Some((start_crop - from_start - remaining, end_crop - from_end))
}

fn snap_output_to_even(mut rect: CropRect, source: CropDetectSource) -> Result<CropRect> {
    let dims = output_dimensions(rect, source)?;
    if dims.width % 2 != 0 {
        rect.right = rect.right.saturating_add(1);
    }
    if dims.height % 2 != 0 {
        rect.bottom = rect.bottom.saturating_add(1);
    }
    Ok(rect)
}

fn invalid_crop(message: &str) -> VoomError {
    VoomError::ToolExecution {
        tool: "ffmpeg".into(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> CropDetectSource {
        CropDetectSource::new(1920, 1080, 7200.0)
    }

    fn config(settings: &CropSettings) -> CropDetectConfig {
        CropDetectConfig::from(settings)
    }

    #[test]
    fn parses_cropdetect_lines_into_edge_crop() {
        let stderr = "[Parsed_cropdetect_0 @ 0x123] crop=1920:816:0:132\n\
                      [Parsed_cropdetect_0 @ 0x123] crop=1918:816:2:132";

        let rects = parse_cropdetect_rects(stderr, source()).unwrap();

        assert_eq!(rects[0], CropRect::new(0, 132, 0, 132));
        assert_eq!(rects[1], CropRect::new(2, 132, 0, 132));
    }

    #[test]
    fn conservative_crop_uses_smallest_edge_crop_across_samples() {
        let rects = [
            CropRect::new(0, 132, 0, 132),
            CropRect::new(0, 138, 0, 126),
            CropRect::new(10, 132, 8, 132),
        ];

        let crop = conservative_crop(&rects, source(), &config(&CropSettings::auto()))
            .unwrap()
            .expect("crop");

        assert_eq!(crop, CropRect::new(0, 132, 0, 126));
    }

    #[test]
    fn minimum_crop_discards_tiny_edge_changes() {
        let rects = [CropRect::new(2, 6, 3, 8)];
        let mut settings = CropSettings::auto();
        settings.minimum_crop = Some(4);

        let crop = conservative_crop(&rects, source(), &config(&settings))
            .unwrap()
            .expect("crop");

        assert_eq!(crop, CropRect::new(0, 6, 0, 8));
    }

    #[test]
    fn preserve_bottom_pixels_reduces_bottom_crop() {
        let rects = [CropRect::new(0, 132, 0, 132)];
        let mut settings = CropSettings::auto();
        settings.preserve_bottom_pixels = Some(60);

        let crop = conservative_crop(&rects, source(), &config(&settings))
            .unwrap()
            .expect("crop");

        assert_eq!(crop, CropRect::new(0, 132, 0, 72));
    }

    #[test]
    fn aspect_lock_expands_height_without_cropping_deeper() {
        let rects = [CropRect::new(0, 132, 0, 132)];
        let mut settings = CropSettings::auto();
        settings.aspect_lock = vec!["21/9".to_string()];

        let crop = conservative_crop(&rects, source(), &config(&settings))
            .unwrap()
            .expect("crop");

        assert_eq!(crop, CropRect::new(0, 128, 0, 128));
    }

    #[test]
    fn aspect_lock_expands_width_when_source_has_room() {
        let rects = [CropRect::new(300, 0, 300, 0)];
        let mut settings = CropSettings::auto();
        settings.aspect_lock = vec!["4/3".to_string()];

        let crop = conservative_crop(&rects, source(), &config(&settings))
            .unwrap()
            .expect("crop");

        assert_eq!(crop, CropRect::new(240, 0, 240, 0));
    }

    #[test]
    fn aspect_lock_ignores_unreachable_ratio() {
        let rects = [CropRect::new(0, 132, 0, 132)];
        let mut settings = CropSettings::auto();
        settings.aspect_lock = vec!["1/1".to_string()];

        let crop = conservative_crop(&rects, source(), &config(&settings))
            .unwrap()
            .expect("crop");

        assert_eq!(crop, CropRect::new(0, 132, 0, 132));
    }

    #[test]
    fn sample_starts_cover_start_middle_and_end() {
        let starts = sample_starts(7200.0, 3, 60);

        assert_eq!(starts, vec![0.0, 3570.0, 7140.0]);
    }
}
