use anyhow::{Context, Result};
use console::style;
use serde_json::Value;

use crate::cli::PolicyCommands;

pub fn run(cmd: PolicyCommands) -> Result<()> {
    match cmd {
        PolicyCommands::List => list(),
        PolicyCommands::Validate { file } => validate(file),
        PolicyCommands::Show { file } => show(file),
        PolicyCommands::Format { file } => format(file),
        PolicyCommands::Diff { a, b } => diff(a, b),
    }
}

fn list() -> Result<()> {
    // Scan standard policy directories
    let config_dir = crate::config::policies_dir();

    if !config_dir.exists() {
        println!("{}", style("No policies directory found.").dim());
        println!("Create policies in: {}", style(config_dir.display()).cyan());
        return Ok(());
    }

    let mut found = false;
    for entry in std::fs::read_dir(&config_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "voom") {
            let name = path
                .file_stem()
                .expect("file has .voom extension so stem exists")
                .to_string_lossy();
            match voom_dsl::compile_policy(&std::fs::read_to_string(&path)?) {
                Ok(policy) => {
                    println!(
                        "  {} {} ({} phases)",
                        style("OK").green(),
                        style(&policy.name).bold(),
                        policy.phases.len()
                    );
                }
                Err(e) => {
                    println!("  {} {} — {e}", style("ERR").red(), style(&name).bold());
                }
            }
            found = true;
        }
    }

    if !found {
        println!("{}", style("No .voom policy files found.").dim());
    }

    Ok(())
}

fn validate(file: std::path::PathBuf) -> Result<()> {
    let file = crate::config::resolve_policy_path(&file);
    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read: {}", file.display()))?;

    match voom_dsl::compile_policy(&source) {
        Ok(policy) => {
            println!(
                "{} Policy \"{}\" is valid ({} phases, {} phase order: [{}])",
                style("OK").bold().green(),
                policy.name,
                policy.phases.len(),
                policy.phase_order.len(),
                policy.phase_order.join(", ")
            );
        }
        Err(e) => {
            anyhow::bail!("Policy validation failed: {e}");
        }
    }

    Ok(())
}

fn show(file: std::path::PathBuf) -> Result<()> {
    let file = crate::config::resolve_policy_path(&file);
    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read: {}", file.display()))?;

    let compiled = voom_dsl::compile_policy(&source).context("policy compilation failed")?;

    println!(
        "{} \"{}\"",
        style("Policy").bold(),
        style(&compiled.name).cyan()
    );
    println!();

    // Config
    println!("{}", style("Config:").bold());
    println!(
        "  Audio languages: [{}]",
        compiled.config.audio_languages.join(", ")
    );
    println!(
        "  Subtitle languages: [{}]",
        compiled.config.subtitle_languages.join(", ")
    );
    println!("  On error: {:?}", compiled.config.on_error);
    if compiled.config.keep_backups {
        println!("  Keep backups: true");
    }
    println!();

    // Phases
    println!(
        "{} (order: [{}])",
        style("Phases:").bold(),
        compiled.phase_order.join(" → ")
    );
    for phase in &compiled.phases {
        println!("  {} {}", style("▸").cyan(), style(&phase.name).bold());
        if !phase.depends_on.is_empty() {
            println!("    depends_on: [{}]", phase.depends_on.join(", "));
        }
        if phase.skip_when.is_some() {
            println!("    skip_when: (condition)");
        }
        if phase.run_if.is_some() {
            println!("    run_if: (condition)");
        }
        println!("    on_error: {:?}", phase.on_error);
        println!("    operations: {}", phase.operations.len());
    }

    // JSON output for full details
    println!();
    println!(
        "{}",
        serde_json::to_string_pretty(&compiled).unwrap_or_else(|_| "Failed to serialize".into())
    );

    Ok(())
}

fn format(file: std::path::PathBuf) -> Result<()> {
    let file = crate::config::resolve_policy_path(&file);
    let source = std::fs::read_to_string(&file)
        .with_context(|| format!("Failed to read: {}", file.display()))?;

    let ast = voom_dsl::parse_policy(&source).context("policy parse failed")?;
    let formatted = voom_dsl::format_policy(&ast);

    std::fs::write(&file, &formatted)
        .with_context(|| format!("Failed to write: {}", file.display()))?;

    println!(
        "{} Formatted {}",
        style("OK").bold().green(),
        style(file.display()).cyan()
    );

    Ok(())
}

fn diff(a: std::path::PathBuf, b: std::path::PathBuf) -> Result<()> {
    let a_path = crate::config::resolve_policy_path(&a);
    let b_path = crate::config::resolve_policy_path(&b);

    let a_source = std::fs::read_to_string(&a_path)
        .with_context(|| format!("Failed to read: {}", a_path.display()))?;
    let b_source = std::fs::read_to_string(&b_path)
        .with_context(|| format!("Failed to read: {}", b_path.display()))?;

    let a_compiled =
        voom_dsl::compile_policy(&a_source).context("failed to compile first policy")?;
    let b_compiled =
        voom_dsl::compile_policy(&b_source).context("failed to compile second policy")?;

    let mut a_json =
        serde_json::to_value(&a_compiled).context("failed to serialize first policy")?;
    let mut b_json =
        serde_json::to_value(&b_compiled).context("failed to serialize second policy")?;

    // Strip source_hash — it always differs
    if let Value::Object(ref mut m) = a_json {
        m.remove("source_hash");
    }
    if let Value::Object(ref mut m) = b_json {
        m.remove("source_hash");
    }

    let mut lines = Vec::new();
    diff_values("", &a_json, &b_json, &mut lines);

    if lines.is_empty() {
        println!("{} Policies are identical.", style("OK").bold().green());
        return Ok(());
    }

    println!(
        "Policy diff: {} vs {}\n",
        style(a_path.display()).cyan(),
        style(b_path.display()).cyan(),
    );
    print_diff_lines(&lines);

    Ok(())
}

enum DiffLine {
    Added {
        path: String,
        value: Value,
    },
    Removed {
        path: String,
        value: Value,
    },
    Changed {
        path: String,
        old: Value,
        new: Value,
    },
}

fn format_value(v: &Value) -> String {
    match v {
        Value::String(s) => format!("\"{s}\""),
        Value::Null => "null".to_string(),
        Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(format_value).collect();
            format!("[{}]", items.join(", "))
        }
        other => other.to_string(),
    }
}

fn diff_values(path: &str, a: &Value, b: &Value, lines: &mut Vec<DiffLine>) {
    if a == b {
        return;
    }

    match (a, b) {
        (Value::Object(a_map), Value::Object(b_map)) => {
            // Special handling for phases array nested inside objects
            let mut keys: Vec<&String> = a_map.keys().collect();
            for k in b_map.keys() {
                if !a_map.contains_key(k) {
                    keys.push(k);
                }
            }
            keys.sort();

            for key in keys {
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match (a_map.get(key), b_map.get(key)) {
                    (Some(av), Some(bv)) => {
                        diff_values(&child, av, bv, lines);
                    }
                    (Some(av), None) => {
                        lines.push(DiffLine::Removed {
                            path: child,
                            value: av.clone(),
                        });
                    }
                    (None, Some(bv)) => {
                        lines.push(DiffLine::Added {
                            path: child,
                            value: bv.clone(),
                        });
                    }
                    (None, None) => {}
                }
            }
        }
        (Value::Array(a_arr), Value::Array(b_arr))
            if is_named_array(a_arr) || is_named_array(b_arr) =>
        {
            diff_named_arrays(path, a_arr, b_arr, lines);
        }
        _ => {
            lines.push(DiffLine::Changed {
                path: path.to_string(),
                old: a.clone(),
                new: b.clone(),
            });
        }
    }
}

/// Check if an array contains objects with a "name" field (like phases).
fn is_named_array(arr: &[Value]) -> bool {
    arr.first()
        .is_some_and(|v| v.as_object().is_some_and(|o| o.contains_key("name")))
}

/// Diff arrays of named objects by matching on the "name" field.
fn diff_named_arrays(path: &str, a_arr: &[Value], b_arr: &[Value], lines: &mut Vec<DiffLine>) {
    let a_names: Vec<&str> = a_arr
        .iter()
        .filter_map(|v| v.get("name")?.as_str())
        .collect();
    let b_names: Vec<&str> = b_arr
        .iter()
        .filter_map(|v| v.get("name")?.as_str())
        .collect();

    let a_by_name: std::collections::HashMap<&str, &Value> = a_arr
        .iter()
        .filter_map(|v| Some((v.get("name")?.as_str()?, v)))
        .collect();
    let b_by_name: std::collections::HashMap<&str, &Value> = b_arr
        .iter()
        .filter_map(|v| Some((v.get("name")?.as_str()?, v)))
        .collect();

    // Removed items (in a but not b)
    for name in &a_names {
        if !b_by_name.contains_key(name) {
            lines.push(DiffLine::Removed {
                path: format!("{path}[\"{name}\"]"),
                value: Value::String((*name).to_string()),
            });
        }
    }

    // Added items (in b but not a)
    for name in &b_names {
        if !a_by_name.contains_key(name) {
            lines.push(DiffLine::Added {
                path: format!("{path}[\"{name}\"]"),
                value: Value::String((*name).to_string()),
            });
        }
    }

    // Changed items (in both)
    for name in &a_names {
        if let (Some(av), Some(bv)) = (a_by_name.get(name), b_by_name.get(name)) {
            let child = format!("{path}[\"{name}\"]");
            diff_values(&child, av, bv, lines);
        }
    }
}

fn print_diff_lines(lines: &[DiffLine]) {
    let mut last_section = String::new();
    for line in lines {
        // Emit section headers from dotted paths
        let section = match line {
            DiffLine::Added { path, .. }
            | DiffLine::Removed { path, .. }
            | DiffLine::Changed { path, .. } => path.split('.').next().unwrap_or("").to_string(),
        };
        if section != last_section && !section.is_empty() {
            if !last_section.is_empty() {
                println!();
            }
            println!("{}:", style(&section).bold());
            last_section = section;
        }

        match line {
            DiffLine::Added { path, value } => {
                let display_path = strip_section(path);
                println!(
                    "  {} {}: {}",
                    style("+").green().bold(),
                    display_path,
                    style(format_value(value)).green()
                );
            }
            DiffLine::Removed { path, value } => {
                let display_path = strip_section(path);
                println!(
                    "  {} {}: {}",
                    style("-").red().bold(),
                    display_path,
                    style(format_value(value)).red()
                );
            }
            DiffLine::Changed { path, old, new } => {
                let display_path = strip_section(path);
                println!(
                    "  {} {}: {} -> {}",
                    style("~").yellow().bold(),
                    display_path,
                    style(format_value(old)).red(),
                    style(format_value(new)).green()
                );
            }
        }
    }
}

/// Strip the top-level section prefix from a dotted path for display.
fn strip_section(path: &str) -> &str {
    path.find('.').map_or(path, |i| &path[i + 1..])
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_POLICY: &str = r#"
policy "test-policy" {
  config {
    languages audio: [eng]
    languages subtitle: [eng]
  }
  phase normalize {
    keep audio where lang in [eng]
    keep subtitles where lang in [eng]
  }
}
"#;

    #[test]
    fn validate_valid_policy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.voom");
        std::fs::write(&file, MINIMAL_POLICY).unwrap();

        // validate reads the file and calls voom_dsl::compile_policy
        let result = validate(file);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_invalid_policy_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.voom");
        std::fs::write(&file, "not a valid policy at all").unwrap();

        // validate() returns Err for invalid policies, so we test indirectly via compile
        let source = std::fs::read_to_string(&file).unwrap();
        assert!(voom_dsl::compile_policy(&source).is_err());
    }

    #[test]
    fn validate_nonexistent_file_returns_error() {
        let result = validate(std::path::PathBuf::from("/nonexistent/test.voom"));
        assert!(result.is_err());
    }

    #[test]
    fn format_valid_policy_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.voom");
        std::fs::write(&file, MINIMAL_POLICY).unwrap();

        let result = format(file.clone());
        assert!(result.is_ok());

        // Verify the file was rewritten
        let formatted = std::fs::read_to_string(&file).unwrap();
        assert!(formatted.contains("policy"));
        assert!(formatted.contains("test-policy"));
    }

    #[test]
    fn format_nonexistent_file_returns_error() {
        let result = format(std::path::PathBuf::from("/nonexistent/test.voom"));
        assert!(result.is_err());
    }

    #[test]
    fn show_valid_policy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.voom");
        std::fs::write(&file, MINIMAL_POLICY).unwrap();

        let result = show(file);
        assert!(result.is_ok());
    }

    #[test]
    fn show_nonexistent_file_returns_error() {
        let result = show(std::path::PathBuf::from("/nonexistent/test.voom"));
        assert!(result.is_err());
    }

    #[test]
    fn show_invalid_policy_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.voom");
        std::fs::write(&file, "garbage content here").unwrap();

        let result = show(file);
        assert!(result.is_err());
    }

    // ── Diff tests ──────────────────────────────────────────

    fn compile_to_json(source: &str) -> Value {
        let compiled = voom_dsl::compile_policy(source).unwrap();
        let mut json = serde_json::to_value(&compiled).unwrap();
        if let Value::Object(ref mut m) = json {
            m.remove("source_hash");
        }
        json
    }

    fn collect_diff(a_src: &str, b_src: &str) -> Vec<DiffLine> {
        let a = compile_to_json(a_src);
        let b = compile_to_json(b_src);
        let mut lines = Vec::new();
        diff_values("", &a, &b, &mut lines);
        lines
    }

    #[test]
    fn diff_identical_policies() {
        let lines = collect_diff(MINIMAL_POLICY, MINIMAL_POLICY);
        assert!(
            lines.is_empty(),
            "identical policies should produce no diff"
        );
    }

    #[test]
    fn diff_config_change() {
        let b = r#"
policy "test-policy" {
  config {
    languages audio: [eng, jpn]
    languages subtitle: [eng]
  }
  phase normalize {
    keep audio where lang in [eng]
    keep subtitles where lang in [eng]
  }
}
"#;
        let lines = collect_diff(MINIMAL_POLICY, b);
        assert!(!lines.is_empty(), "config change should produce diff");
        let has_audio_lang_change = lines.iter().any(|l| {
            matches!(l, DiffLine::Changed { path, .. }
                if path.contains("audio_languages"))
        });
        assert!(
            has_audio_lang_change,
            "should detect audio_languages change"
        );
    }

    #[test]
    fn diff_added_phase() {
        let b = r#"
policy "test-policy" {
  config {
    languages audio: [eng]
    languages subtitle: [eng]
  }
  phase normalize {
    keep audio where lang in [eng]
    keep subtitles where lang in [eng]
  }
  phase cleanup {
    remove subtitles where commentary
  }
}
"#;
        let lines = collect_diff(MINIMAL_POLICY, b);
        let has_added = lines.iter().any(|l| {
            matches!(l, DiffLine::Added { path, .. }
                if path.contains("cleanup"))
        });
        assert!(has_added, "should detect added phase");
    }

    #[test]
    fn diff_removed_phase() {
        let a = r#"
policy "test-policy" {
  config {
    languages audio: [eng]
    languages subtitle: [eng]
  }
  phase normalize {
    keep audio where lang in [eng]
    keep subtitles where lang in [eng]
  }
  phase cleanup {
    remove subtitles where commentary
  }
}
"#;
        let lines = collect_diff(a, MINIMAL_POLICY);
        let has_removed = lines.iter().any(|l| {
            matches!(l, DiffLine::Removed { path, .. }
                if path.contains("cleanup"))
        });
        assert!(has_removed, "should detect removed phase");
    }

    #[test]
    fn diff_changed_operation() {
        let a = r#"
policy "test-policy" {
  config {
    languages audio: [eng]
    languages subtitle: [eng]
  }
  phase normalize {
    keep audio where lang in [eng]
    keep subtitles where lang in [eng]
  }
}
"#;
        let b = r#"
policy "test-policy" {
  config {
    languages audio: [eng]
    languages subtitle: [eng]
  }
  phase normalize {
    keep audio where lang in [eng, jpn]
    keep subtitles where lang in [eng]
  }
}
"#;
        let lines = collect_diff(a, b);
        assert!(!lines.is_empty(), "changed operation should produce diff");
    }

    #[test]
    fn diff_file_integration() {
        let dir = tempfile::tempdir().unwrap();
        let a_file = dir.path().join("a.voom");
        let b_file = dir.path().join("b.voom");
        std::fs::write(&a_file, MINIMAL_POLICY).unwrap();
        std::fs::write(&b_file, MINIMAL_POLICY).unwrap();

        let result = diff(a_file, b_file);
        assert!(result.is_ok());
    }

    #[test]
    fn diff_nonexistent_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let a_file = dir.path().join("a.voom");
        std::fs::write(&a_file, MINIMAL_POLICY).unwrap();

        let result = diff(a_file, std::path::PathBuf::from("/nonexistent/b.voom"));
        assert!(result.is_err());
    }
}
