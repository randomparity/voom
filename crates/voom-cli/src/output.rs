//! Output formatting utilities for the CLI.

use std::path::Path;

use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Cell, ContentArrangement, Table};
use console::style;
use voom_domain::media::{MediaFile, Track};
use voom_domain::transition::{FileTransition, TransitionSource};
use voom_domain::utils::format;

use crate::cli::OutputFormat;

/// Strip control characters (ANSI escapes, newlines, null bytes, etc.)
/// from a string before displaying it in the terminal.
///
/// Config values and external process output are untrusted input that could
/// contain injected escape sequences. Call this at the display boundary.
pub fn sanitize_for_display(s: &str) -> String {
    let stripped = console::strip_ansi_codes(s);
    stripped.chars().filter(|c| !c.is_control()).collect()
}

/// Returns the current terminal width, defaulting to 80 if it cannot be determined.
pub fn term_width() -> usize {
    console::Term::stdout().size().1 as usize
}

/// Shrink a filename to fit within `max_len` characters.
///
/// Preserves the beginning of the stem and the file extension, joining them
/// with "..." when truncation is needed.
pub fn shrink_filename(name: &str, max_len: usize) -> String {
    if name.len() <= max_len {
        return name.to_string();
    }

    let path = Path::new(name);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // "..." + ext  (the dots replace the dot separator)
    let suffix_len = 3 + ext.len(); // e.g., "...mkv" = 6

    if max_len <= suffix_len + 1 {
        // Not enough room for even 1 char + suffix; just hard-truncate
        return name.chars().take(max_len).collect();
    }

    let prefix_len = max_len - suffix_len;
    let prefix: String = name.chars().take(prefix_len).collect();

    if ext.is_empty() {
        format!("{prefix}...")
    } else {
        format!("{prefix}...{ext}")
    }
}

/// Fixed-width overhead of the standard progress bar template:
/// `{spinner} [{bar:40}] {pos}/{len} ({percent}%) {msg}`
///
/// Breakdown: spinner(1) + space(1) + bracket(1) + bar(40) + bracket(1)
///   + space(1) + pos/len(≤11) + space(1) + percent(≤7) + space(1)
///   + safety margin(2) = 67, rounded up to 68.
pub const PROGRESS_FIXED_WIDTH: usize = 68;

/// Compute the max filename length that keeps a progress line on one terminal row.
///
/// `fixed_width` is the number of characters used by the non-filename parts of
/// the progress line (spinner, bar, counters, ETA text, etc.).
pub fn max_filename_len(fixed_width: usize) -> usize {
    let width = term_width();
    width.saturating_sub(fixed_width).max(12)
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
pub fn truncate_with_ellipsis(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let target = max_len.saturating_sub(3);
        let end = floor_char_boundary(s, target);
        format!("{}...", &s[..end])
    }
}

/// Find the largest byte index <= `index` that is a char boundary in `s`.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Format skip reasons sorted by frequency, showing at most `limit` entries.
pub fn format_skip_reasons(
    reasons: &std::collections::HashMap<String, u64>,
    limit: usize,
) -> String {
    if reasons.is_empty() {
        return String::new();
    }
    let mut sorted: Vec<_> = reasons.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    sorted
        .iter()
        .take(limit)
        .map(|(reason, count)| {
            let display = truncate_with_ellipsis(reason, 30);
            format!("{display} ({count})")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Format a list of discovered files as a table.
pub fn format_scan_results(
    files: &[(std::path::PathBuf, u64, Option<String>)],
    format: OutputFormat,
) {
    match format {
        OutputFormat::Json => {
            let json: Vec<serde_json::Value> = files
                .iter()
                .map(|(path, size, hash)| {
                    serde_json::json!({
                        "path": path.display().to_string(),
                        "size": size,
                        "hash": hash,
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json)
                    .expect("serde_json::Value serialization cannot fail")
            );
        }
        OutputFormat::Table => {
            let mut table = new_table();
            table.set_header(vec!["Path", "Size", "Hash"]);
            for (path, size, hash) in files {
                let hash_str = hash.as_deref().unwrap_or("—");
                let hash_preview = hash_preview(hash_str);
                table.add_row(vec![
                    Cell::new(path.display()),
                    Cell::new(format::format_size(*size)),
                    Cell::new(hash_preview),
                ]);
            }
            println!("{table}");
        }
        OutputFormat::Plain => {
            for (path, _, _) in files {
                println!("{}", path.display());
            }
        }
    }
}

/// Format a media file's metadata as a table.
pub fn format_file_info(file: &MediaFile, tracks_only: bool) {
    if !tracks_only {
        println!("{}", style("File Information").bold());
        let mut table = new_table();
        table.set_header(vec!["Property", "Value"]);
        table.add_row(vec!["Path", &file.path.display().to_string()]);
        table.add_row(vec!["Container", file.container.as_str()]);
        table.add_row(vec!["Size", &format::format_size(file.size)]);
        table.add_row(vec!["Duration", &format::format_duration(file.duration)]);
        if let Some(br) = file.bitrate {
            table.add_row(vec!["Bitrate", &format!("{} kbps", br / 1000)]);
        }
        table.add_row(vec!["Hash", file.content_hash.as_deref().unwrap_or("—")]);
        table.add_row(vec!["ID", &file.id.to_string()]);
        println!("{table}");
        println!();
    }

    println!("{}", style("Tracks").bold());
    format_tracks(&file.tracks);
}

/// Format a media file as JSON.
pub fn format_file_json(file: &MediaFile) {
    println!(
        "{}",
        serde_json::to_string_pretty(file).expect("MediaFile serialization cannot fail")
    );
}

/// Render a transitions list as a formatted table string.
/// Returns an empty string if `transitions` is empty.
pub fn render_transitions_table(transitions: &[FileTransition]) -> String {
    use crate::commands::history::format_snapshot_cell;

    if transitions.is_empty() {
        return String::new();
    }

    let has_lineage =
        transitions.len() > 1 && transitions.windows(2).any(|w| w[0].file_id != w[1].file_id);

    let mut table = new_table();
    if has_lineage {
        table.set_header(vec![
            "#",
            "Date",
            "Source",
            "File ID",
            "Size",
            "From Hash",
            "To Hash",
            "Media",
        ]);
    } else {
        table.set_header(vec![
            "#",
            "Date",
            "Source",
            "Size",
            "From Hash",
            "To Hash",
            "Media",
        ]);
    }

    let col_count = if has_lineage { 8 } else { 7 };
    let mut prev_file_id: Option<uuid::Uuid> = None;

    for (i, t) in transitions.iter().enumerate() {
        if has_lineage {
            if let Some(prev) = prev_file_id {
                if prev != t.file_id {
                    let sep = style("── external modification ──").dim().to_string();
                    let mut sep_row: Vec<Cell> = vec![Cell::new(""), Cell::new(&sep)];
                    for _ in 2..col_count {
                        sep_row.push(Cell::new(""));
                    }
                    table.add_row(sep_row);
                }
            }
            prev_file_id = Some(t.file_id);
        }

        let date = format::format_display(&t.created_at);
        let from = t
            .from_hash
            .as_deref()
            .map(hash_preview)
            .unwrap_or("\u{2014}");
        let to = hash_preview(&t.to_hash);

        let source_display = match (&t.source, &t.phase_name, &t.outcome) {
            (TransitionSource::Voom, Some(phase), Some(outcome)) => {
                format!("voom:{phase} ({})", outcome.as_str())
            }
            _ => t.source.as_str().to_string(),
        };

        let size_cell = match t.from_size {
            Some(from_sz) => {
                let delta = t.to_size as i64 - from_sz as i64;
                let formatted = format::format_size(t.to_size);
                if delta == 0 {
                    formatted
                } else if delta < 0 {
                    format!("{formatted} (-{})", format::format_size((-delta) as u64))
                } else {
                    format!("{formatted} (+{})", format::format_size(delta as u64))
                }
            }
            None => format::format_size(t.to_size),
        };

        let mut row = vec![Cell::new(i + 1), Cell::new(date), Cell::new(source_display)];

        if has_lineage {
            let short_id = &t.file_id.to_string()[..8];
            row.push(Cell::new(format!("{short_id}...")));
        }

        row.push(Cell::new(size_cell));
        row.push(Cell::new(from));
        row.push(Cell::new(to));

        let media_cell = t
            .metadata_snapshot
            .as_ref()
            .map(format_snapshot_cell)
            .unwrap_or_else(|| "\u{2014}".to_string());
        row.push(Cell::new(media_cell));

        table.add_row(row);
    }

    table.to_string()
}

/// Format tracks as a table.
pub fn format_tracks(tracks: &[Track]) {
    let mut table = new_table();
    table.set_header(vec![
        "#", "Type", "Codec", "Language", "Title", "Default", "Forced", "Details",
    ]);

    for track in tracks {
        let details = track_details(track);
        table.add_row(vec![
            Cell::new(track.index),
            Cell::new(track.track_type.as_str()),
            Cell::new(&track.codec),
            Cell::new(&track.language),
            Cell::new(&track.title),
            Cell::new(if track.is_default { "yes" } else { "" }),
            Cell::new(if track.is_forced { "yes" } else { "" }),
            Cell::new(details),
        ]);
    }

    println!("{table}");
}

/// Build a details string for a track (resolution, channels, etc.).
fn track_details(track: &Track) -> String {
    let mut parts = Vec::new();

    if let (Some(w), Some(h)) = (track.width, track.height) {
        parts.push(format!("{w}x{h}"));
    }
    if let Some(fps) = track.frame_rate {
        parts.push(format!("{fps:.2}fps"));
    }
    if track.is_hdr {
        if let Some(ref fmt) = track.hdr_format {
            parts.push(fmt.clone());
        } else {
            parts.push("HDR".into());
        }
    }
    if let Some(ch) = track.channels {
        parts.push(format!("{ch}ch"));
    }
    if let Some(ref layout) = track.channel_layout {
        parts.push(layout.clone());
    }
    if let Some(sr) = track.sample_rate {
        parts.push(format!("{sr}Hz"));
    }

    parts.join(", ")
}

/// Entry for the plugin list table.
pub struct PluginListEntry {
    pub name: String,
    pub version: String,
    pub description: String,
    pub capabilities: Vec<String>,
}

/// Format a list of plugins as a table.
pub fn format_plugin_list(plugins: &[PluginListEntry]) {
    let mut table = new_table();
    table.set_header(vec!["Plugin", "Version", "Description", "Capabilities"]);
    for entry in plugins {
        table.add_row(vec![
            Cell::new(&entry.name),
            Cell::new(&entry.version),
            Cell::new(&entry.description),
            Cell::new(entry.capabilities.join(", ")),
        ]);
    }
    println!("{table}");
}

/// Prompt the user to type "yes" to confirm a destructive action.
///
/// When `skip` is true, returns `Ok(true)` immediately (for `--yes` flags).
/// Otherwise prints `prompt` to stderr, reads a line from stdin, and returns
/// `true` only if the user typed exactly "yes".
pub fn confirm(prompt: &str, skip: bool) -> std::io::Result<bool> {
    if skip {
        return Ok(true);
    }
    eprintln!("{prompt}");
    eprintln!("Type 'yes' to confirm:");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim() == "yes")
}

/// Print executor capabilities (codecs, HW accel, formats) for a plugin.
pub fn format_executor_capabilities(
    name: &str,
    capabilities: &voom_domain::capability_map::CapabilityMap,
) {
    if let Some(caps) = capabilities.executor_capabilities(name) {
        if !caps.hw_accels.is_empty() {
            let best = capabilities.best_hwaccel();
            println!("{}", style("Hardware Acceleration:").bold());
            println!(
                "  {} {} ({})",
                style("Backend:").bold(),
                style(best).green(),
                caps.hw_accels.join(", ")
            );
        }
        print_codec_section(&caps.codecs);
        print_format_section(&caps.formats);
    }
}

/// Print the codec subsections (decoders, encoders, HW decoders, HW encoders).
fn print_codec_section(codecs: &voom_domain::events::CodecCapabilities) {
    if codecs.decoders.is_empty() && codecs.encoders.is_empty() {
        return;
    }
    println!("{}", style("Codecs:").bold());

    let sections: &[(&str, &[String])] = &[
        ("Decoders", &codecs.decoders),
        ("Encoders", &codecs.encoders),
        ("HW Decoders", &codecs.hw_decoders),
        ("HW Encoders", &codecs.hw_encoders),
    ];

    for (label, items) in sections {
        if !items.is_empty() {
            println!(
                "  {} ({}): {}",
                style(label).bold(),
                items.len(),
                items.join(", ")
            );
        }
    }
}

/// Print the supported formats section.
fn print_format_section(formats: &[String]) {
    if !formats.is_empty() {
        println!(
            "{} ({}): {}",
            style("Formats:").bold(),
            formats.len(),
            formats.join(", ")
        );
    }
}

/// Truncate a hash to a 12-character preview for table display.
pub fn hash_preview(hash: &str) -> &str {
    if hash.len() >= 12 {
        &hash[..12]
    } else {
        hash
    }
}

/// Create a table with the standard VOOM style.
pub fn new_table() -> Table {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic);
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_passes_normal_strings() {
        assert_eq!(sanitize_for_display("hello world"), "hello world");
        assert_eq!(sanitize_for_display("GPU 0: RTX 4090"), "GPU 0: RTX 4090");
    }

    #[test]
    fn test_sanitize_strips_ansi_escapes() {
        // CSI sequence: ESC [ 31 m (red text) — fully stripped, not just ESC byte
        assert_eq!(sanitize_for_display("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn test_sanitize_strips_control_chars() {
        assert_eq!(sanitize_for_display("a\x00b\nc\rd\te"), "abcde");
    }

    #[test]
    fn test_sanitize_empty_string() {
        assert_eq!(sanitize_for_display(""), "");
    }

    #[test]
    fn test_sanitize_only_control_chars() {
        assert_eq!(sanitize_for_display("\x00\n\r\t\x1b"), "");
    }

    #[test]
    fn test_shrink_no_truncation_needed() {
        assert_eq!(shrink_filename("short.mkv", 40), "short.mkv");
    }

    #[test]
    fn test_shrink_exact_length() {
        let name = "x".repeat(30) + ".mkv";
        assert_eq!(shrink_filename(&name, 34), name);
    }

    #[test]
    fn test_shrink_long_name_preserves_extension() {
        let result = shrink_filename(
            "A Very Long Movie Name (2025) - S01E01 - Episode Title [WEBDL-1080p]-GROUP.mkv",
            40,
        );
        assert_eq!(result.len(), 40);
        assert!(result.ends_with("...mkv"), "got: {result}");
        assert!(result.starts_with("A Very Long Movie Name (2025) - S"));
    }

    #[test]
    fn test_shrink_no_extension() {
        let result = shrink_filename("a_very_long_filename_without_extension", 20);
        assert_eq!(result.len(), 20);
        assert!(result.ends_with("..."), "got: {result}");
    }

    #[test]
    fn test_shrink_very_small_max() {
        let result = shrink_filename("movie.mkv", 5);
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn test_shrink_various_extensions() {
        let result = shrink_filename("Some Long Name Here.m2ts", 20);
        assert!(result.ends_with("...m2ts"), "got: {result}");
        assert_eq!(result.len(), 20);
    }

    #[test]
    fn test_truncate_with_ellipsis_ascii() {
        assert_eq!(truncate_with_ellipsis("hello", 10), "hello");
        assert_eq!(truncate_with_ellipsis("hello world", 8), "hello...");
    }

    #[test]
    fn test_truncate_with_ellipsis_multibyte_no_panic() {
        // "日本語テスト" — each char is 3 bytes
        let s = "日本語テスト";
        let result = truncate_with_ellipsis(s, 10);
        assert!(result.ends_with("..."), "got: {result}");
        // Must not panic and must be valid UTF-8 (implicit if we get here)
    }

    #[test]
    fn test_truncate_with_ellipsis_emoji() {
        let s = "🎬🎥🎞️🎦📽️ movie";
        let result = truncate_with_ellipsis(s, 8);
        assert!(result.ends_with("..."), "got: {result}");
    }
}

#[cfg(test)]
mod transition_table_tests {
    use super::*;
    use std::path::PathBuf;
    use uuid::Uuid;
    use voom_domain::transition::{FileTransition, TransitionSource};

    fn make_transition(source: TransitionSource, to_size: u64) -> FileTransition {
        FileTransition::new(
            Uuid::new_v4(),
            PathBuf::from("/test/video.mkv"),
            "abc123".to_string(),
            to_size,
            source,
        )
    }

    #[test]
    fn render_transitions_table_nonempty() {
        let transitions = vec![
            make_transition(TransitionSource::Discovery, 1_000_000),
            make_transition(TransitionSource::Voom, 950_000),
        ];
        let output = render_transitions_table(&transitions);
        assert!(!output.is_empty());
        assert!(output.contains("discovery"));
    }

    #[test]
    fn render_transitions_table_empty_returns_empty_string() {
        let output = render_transitions_table(&[]);
        assert!(output.is_empty());
    }

    #[test]
    fn render_transitions_table_shows_lineage_separator() {
        let file_id_a = Uuid::new_v4();
        let file_id_b = Uuid::new_v4();

        let mut t1 = make_transition(TransitionSource::Discovery, 1_000_000);
        t1.file_id = file_id_a;

        let mut t2 = make_transition(TransitionSource::Voom, 950_000);
        t2.file_id = file_id_b;

        let output = render_transitions_table(&[t1, t2]);
        assert!(output.contains("external modification"));
        assert!(output.contains("File ID"));
    }

    #[test]
    fn render_transitions_table_enriches_voom_source() {
        use voom_domain::stats::ProcessingOutcome;

        let mut t = make_transition(TransitionSource::Voom, 950_000);
        t.phase_name = Some("normalize".to_string());
        t.outcome = Some(ProcessingOutcome::Success);

        let output = render_transitions_table(&[t]);
        assert!(output.contains("voom:normalize (success)"));
    }
}
