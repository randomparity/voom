//! `FFmpeg` progress parsing from `-progress pipe:1` output and stderr.

/// Parsed progress information from `FFmpeg` output.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ProgressInfo {
    pub frame: Option<u64>,
    pub fps: Option<f64>,
    pub bitrate: Option<String>,
    pub total_size: Option<u64>,
    pub out_time_us: Option<u64>,
    pub speed: Option<f64>,
    pub progress: ProgressState,
}

/// State of `FFmpeg` encoding progress.
#[derive(Debug, Clone, PartialEq)]
pub enum ProgressState {
    Continue,
    End,
}

impl ProgressInfo {
    fn new() -> Self {
        Self {
            frame: None,
            fps: None,
            bitrate: None,
            total_size: None,
            out_time_us: None,
            speed: None,
            progress: ProgressState::Continue,
        }
    }
}

/// Parse `FFmpeg` `-progress pipe:1` output (key=value lines) into `ProgressInfo`.
///
/// `FFmpeg` emits blocks of key=value pairs separated by `progress=continue` or
/// `progress=end` lines. This function parses the last complete block.
#[must_use]
pub fn parse_progress(output: &str) -> Option<ProgressInfo> {
    if output.trim().is_empty() {
        return None;
    }

    let mut info = ProgressInfo::new();
    let mut found_any = false;

    for line in output.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            found_any = true;

            match key {
                "frame" => {
                    info.frame = value.parse().ok();
                }
                "fps" => {
                    info.fps = value.parse().ok();
                }
                "bitrate" => {
                    if value != "N/A" {
                        info.bitrate = Some(value.to_string());
                    }
                }
                "total_size" => {
                    info.total_size = value.parse().ok();
                }
                "out_time_us" => {
                    info.out_time_us = value.parse().ok();
                }
                "out_time_ms" => {
                    // out_time_ms is actually in microseconds in FFmpeg's output
                    info.out_time_us = value.parse().ok();
                }
                "speed" => {
                    let speed_str = value.trim_end_matches('x');
                    info.speed = speed_str.parse().ok();
                }
                "progress" => {
                    info.progress = if value == "end" {
                        ProgressState::End
                    } else {
                        ProgressState::Continue
                    };
                }
                _ => {}
            }
        }
    }

    if found_any {
        Some(info)
    } else {
        None
    }
}

/// Calculate completion percentage given progress time and total duration.
///
/// Returns a value between 0.0 and 100.0.
#[must_use]
pub fn completion_percentage(out_time_us: u64, total_duration_secs: f64) -> f64 {
    if total_duration_secs <= 0.0 {
        return 0.0;
    }
    let out_time_secs = out_time_us as f64 / 1_000_000.0;
    let pct = (out_time_secs / total_duration_secs) * 100.0;
    pct.clamp(0.0, 100.0)
}

/// Parse `FFmpeg` stderr progress line.
///
/// `FFmpeg` stderr lines look like:
/// `frame=  120 fps= 30 q=28.0 size=    1024kB time=00:00:04.00 bitrate=2097.2kbits/s speed=1.5x`
#[must_use]
pub fn parse_stderr_progress(line: &str) -> Option<ProgressInfo> {
    let line = line.trim();
    if !line.contains("frame=") || !line.contains("time=") {
        return None;
    }

    let mut info = ProgressInfo::new();

    // Parse frame=N
    if let Some(frame_str) = extract_value(line, "frame=") {
        info.frame = frame_str.trim().parse().ok();
    }

    // Parse fps=N
    if let Some(fps_str) = extract_value(line, "fps=") {
        info.fps = fps_str.trim().parse().ok();
    }

    // Parse size=NkB
    if let Some(size_str) = extract_value(line, "size=") {
        let size_str = size_str.trim();
        if let Some(kb_str) = size_str.strip_suffix("kB") {
            if let Ok(kb) = kb_str.trim().parse::<u64>() {
                info.total_size = Some(kb * 1024);
            }
        }
    }

    // Parse time=HH:MM:SS.ff
    if let Some(time_str) = extract_value(line, "time=") {
        if let Some(us) = parse_time_to_us(time_str.trim()) {
            info.out_time_us = Some(us);
        }
    }

    // Parse bitrate=Nkbits/s
    if let Some(br_str) = extract_value(line, "bitrate=") {
        let br = br_str.trim();
        if br != "N/A" {
            info.bitrate = Some(br.to_string());
        }
    }

    // Parse speed=Nx
    if let Some(speed_str) = extract_value(line, "speed=") {
        let speed_str = speed_str.trim().trim_end_matches('x');
        info.speed = speed_str.parse().ok();
    }

    Some(info)
}

/// Extract a value from an `FFmpeg` stderr key=value segment.
fn extract_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)?;
    let value_start = start + key.len();
    let rest = &line[value_start..];

    // Find the end: next key= pattern or end of string
    // Keys are preceded by a space and are alphabetic
    let end = rest
        .find(|c: char| c.is_ascii_alphabetic())
        .and_then(|pos| {
            // Check if this looks like the start of a new key (followed by =)
            let after = &rest[pos..];
            if after.contains('=') {
                // Walk back to find the space before this key
                let before = &rest[..pos];
                // Find the last space before this key start
                before.rfind(' ')
            } else {
                None
            }
        })
        .unwrap_or(rest.len());

    let value = rest[..end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Parse a time string like `00:01:30.50` to microseconds.
fn parse_time_to_us(time: &str) -> Option<u64> {
    let parts: Vec<&str> = time.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let hours: f64 = parts[0].parse().ok()?;
    let minutes: f64 = parts[1].parse().ok()?;
    let seconds: f64 = parts[2].parse().ok()?;
    let total_secs = hours * 3600.0 + minutes * 60.0 + seconds;
    Some((total_secs * 1_000_000.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_progress_output() {
        let output = "\
frame=120
fps=30.00
bitrate=2048.0kbits/s
total_size=1048576
out_time_us=4000000
speed=1.5x
progress=continue
";
        let info = parse_progress(output).unwrap();
        assert_eq!(info.frame, Some(120));
        assert_eq!(info.fps, Some(30.0));
        assert_eq!(info.bitrate, Some("2048.0kbits/s".to_string()));
        assert_eq!(info.total_size, Some(1048576));
        assert_eq!(info.out_time_us, Some(4000000));
        assert_eq!(info.speed, Some(1.5));
        assert_eq!(info.progress, ProgressState::Continue);
    }

    #[test]
    fn test_parse_progress_end() {
        let output = "\
frame=2400
fps=60.00
total_size=52428800
out_time_us=80000000
speed=2.0x
progress=end
";
        let info = parse_progress(output).unwrap();
        assert_eq!(info.progress, ProgressState::End);
        assert_eq!(info.frame, Some(2400));
        assert_eq!(info.speed, Some(2.0));
    }

    #[test]
    fn test_parse_progress_empty() {
        assert!(parse_progress("").is_none());
        assert!(parse_progress("   ").is_none());
    }

    #[test]
    fn test_parse_progress_na_bitrate() {
        let output = "bitrate=N/A\nprogress=continue\n";
        let info = parse_progress(output).unwrap();
        assert!(info.bitrate.is_none());
    }

    #[test]
    fn test_parse_stderr_progress() {
        let line =
            "frame=  120 fps= 30 q=28.0 size=    1024kB time=00:00:04.00 bitrate=2097.2kbits/s speed=1.5x";
        let info = parse_stderr_progress(line).unwrap();
        assert_eq!(info.frame, Some(120));
        assert_eq!(info.fps, Some(30.0));
        assert_eq!(info.out_time_us, Some(4_000_000));
        assert_eq!(info.speed, Some(1.5));
    }

    #[test]
    fn test_parse_stderr_progress_not_progress_line() {
        assert!(parse_stderr_progress("Some random ffmpeg output").is_none());
        assert!(parse_stderr_progress("Input #0, matroska,webm").is_none());
    }

    #[test]
    fn test_completion_percentage() {
        // 50% through a 100-second file
        let pct = completion_percentage(50_000_000, 100.0);
        assert!((pct - 50.0).abs() < 0.01);

        // 100% complete
        let pct = completion_percentage(100_000_000, 100.0);
        assert!((pct - 100.0).abs() < 0.01);

        // Over 100% (clamp)
        let pct = completion_percentage(200_000_000, 100.0);
        assert!((pct - 100.0).abs() < 0.01);

        // 25% through
        let pct = completion_percentage(30_000_000, 120.0);
        assert!((pct - 25.0).abs() < 0.01);
    }

    #[test]
    fn test_completion_percentage_zero_duration() {
        assert_eq!(completion_percentage(5_000_000, 0.0), 0.0);
        assert_eq!(completion_percentage(0, 0.0), 0.0);
        assert_eq!(completion_percentage(5_000_000, -10.0), 0.0);
    }

    #[test]
    fn test_parse_time_to_us() {
        assert_eq!(parse_time_to_us("00:00:01.00"), Some(1_000_000));
        assert_eq!(parse_time_to_us("00:01:00.00"), Some(60_000_000));
        assert_eq!(parse_time_to_us("01:00:00.00"), Some(3_600_000_000));
        assert_eq!(parse_time_to_us("00:00:04.00"), Some(4_000_000));
        assert!(parse_time_to_us("invalid").is_none());
    }
}
