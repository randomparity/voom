use anyhow::{bail, Result};
use console::style;

use crate::cli::ConfigCommands;
use crate::config;

pub fn run(cmd: ConfigCommands) -> Result<()> {
    match cmd {
        ConfigCommands::Show => show(),
        ConfigCommands::Edit => edit(),
        ConfigCommands::Get { key } => get(&key),
        ConfigCommands::Set { key, value } => set(&key, &value),
    }
}

fn show() -> Result<()> {
    let path = config::config_path();

    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        // Redact auth_token value to avoid leaking secrets
        let redacted = contents
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with("auth_token") && trimmed.contains('=') {
                    let prefix =
                        &line[..line.find('=').expect("line contains '=' (checked above)") + 1];
                    format!("{prefix} \"[REDACTED]\"")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        println!(
            "{} {}",
            style("Config:").bold(),
            style(path.display()).dim()
        );
        println!();
        println!("{redacted}");
    } else {
        println!(
            "{} No config file found at {}",
            style("INFO").dim(),
            style(path.display()).cyan()
        );
        println!();
        println!("{}", style("Default configuration:").bold());
        let cfg = config::AppConfig::default();
        println!(
            "{}",
            toml::to_string_pretty(&cfg).unwrap_or_else(|_| "Failed to serialize".into())
        );
    }

    Ok(())
}

fn edit() -> Result<()> {
    let path = config::config_path();

    // Ensure directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Create default config if it doesn't exist
    if !path.exists() {
        let cfg = config::AppConfig::default();
        let contents = toml::to_string_pretty(&cfg)?;
        std::fs::write(&path, contents)?;
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new(&editor).arg(&path).status()?;

    if !status.success() {
        anyhow::bail!("Editor exited with status: {status}");
    }

    // Validate the edited config
    match config::load_config() {
        Ok(_) => println!("{} Config is valid.", style("OK").bold().green()),
        Err(e) => {
            println!(
                "{} Config has errors: {e}",
                style("WARNING").bold().yellow()
            );
        }
    }

    Ok(())
}

fn get(key: &str) -> Result<()> {
    let path = config::config_path();
    let raw = load_raw_toml(&path)?;
    let value = resolve_toml_key(&raw, key)?;
    println!("{}", format_value(&value));
    Ok(())
}

fn set(key: &str, value: &str) -> Result<()> {
    let path = config::config_path();
    let mut raw = load_raw_toml(&path)?;

    let segments: Vec<&str> = key.split('.').collect();
    if segments.is_empty() {
        bail!("key must not be empty");
    }

    // Navigate to the parent table, creating intermediate tables as needed
    let parent = if segments.len() == 1 {
        &mut raw
    } else {
        let mut current = &mut raw;
        for seg in &segments[..segments.len() - 1] {
            current = current
                .entry(*seg)
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .ok_or_else(|| anyhow::anyhow!("'{seg}' is not a table in config"))?;
        }
        current
    };

    let leaf = segments[segments.len() - 1];

    // Determine the coerced value
    let existing = parent.get(leaf);
    let coerced = coerce_value(value, existing)?;

    parent.insert(leaf.to_string(), coerced);

    // Validate by deserializing the full tree into AppConfig
    let toml_str = toml::to_string_pretty(&raw)?;
    let validated: config::AppConfig =
        toml::from_str(&toml_str).map_err(|e| anyhow::anyhow!("invalid config after set: {e}"))?;

    // Save via save_config (handles permissions)
    config::save_config(&validated)?;

    // Reload to confirm
    config::load_config()?;

    println!(
        "{} Set {} = {}",
        style("OK").bold().green(),
        style(key).cyan(),
        style(value).dim(),
    );
    Ok(())
}

/// Load the config file as a raw TOML table.
fn load_raw_toml(path: &std::path::Path) -> Result<toml::Table> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let table: toml::Table = toml::from_str(&contents)
                .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
            Ok(table)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => bail!("failed to read {}: {e}", path.display()),
    }
}

/// Traverse a TOML table by dot-separated key segments.
fn resolve_toml_key(table: &toml::Table, key: &str) -> Result<toml::Value> {
    let segments: Vec<&str> = key.split('.').collect();
    if segments.is_empty() {
        bail!("key must not be empty");
    }

    let mut current = table;
    for (i, seg) in segments[..segments.len() - 1].iter().enumerate() {
        match current.get(*seg) {
            Some(v) => match v.as_table() {
                Some(t) => current = t,
                None => {
                    let path = segments[..=i].join(".");
                    bail!(
                        "'{path}' is not a table; \
                         cannot traverse further"
                    );
                }
            },
            None => {
                let path = segments[..=i].join(".");
                bail!("key not set: {path}");
            }
        }
    }

    let leaf = segments[segments.len() - 1];
    match current.get(leaf) {
        Some(v) => Ok(v.clone()),
        None => {
            bail!("key not set: {key}");
        }
    }
}

/// Coerce a string value, matching the existing type when present.
fn coerce_value(raw: &str, existing: Option<&toml::Value>) -> Result<toml::Value> {
    if let Some(existing) = existing {
        // Reject setting arrays directly
        if existing.is_array() {
            bail!(
                "cannot set array values directly; \
                 use `voom config edit` instead"
            );
        }
        // Match existing type
        match existing {
            toml::Value::Boolean(_) => match raw {
                "true" => {
                    return Ok(toml::Value::Boolean(true));
                }
                "false" => {
                    return Ok(toml::Value::Boolean(false));
                }
                _ => bail!(
                    "expected a boolean (true/false), \
                         got '{raw}'"
                ),
            },
            toml::Value::Integer(_) => {
                let n: i64 = raw
                    .parse()
                    .map_err(|_| anyhow::anyhow!("expected an integer, got '{raw}'"))?;
                return Ok(toml::Value::Integer(n));
            }
            toml::Value::Float(_) => {
                let f: f64 = raw
                    .parse()
                    .map_err(|_| anyhow::anyhow!("expected a float, got '{raw}'"))?;
                return Ok(toml::Value::Float(f));
            }
            toml::Value::String(_) => {
                return Ok(toml::Value::String(raw.to_string()));
            }
            // Tables: reject direct set
            toml::Value::Table(_) => {
                bail!(
                    "cannot set a table directly; \
                     use a more specific key or \
                     `voom config edit`"
                );
            }
            _ => {}
        }
    }

    // Auto-detect type for new keys
    if raw == "true" {
        return Ok(toml::Value::Boolean(true));
    }
    if raw == "false" {
        return Ok(toml::Value::Boolean(false));
    }
    if let Ok(n) = raw.parse::<i64>() {
        return Ok(toml::Value::Integer(n));
    }
    if let Ok(f) = raw.parse::<f64>() {
        return Ok(toml::Value::Float(f));
    }
    Ok(toml::Value::String(raw.to_string()))
}

/// Format a TOML value for output.
/// Scalars → plain text (script-friendly).
/// Tables/arrays → JSON.
fn format_value(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Datetime(dt) => dt.to_string(),
        toml::Value::Array(_) | toml::Value::Table(_) => {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| format!("{value}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    #[test]
    fn test_default_config_serializes_to_valid_toml() {
        let config = config::AppConfig::default();
        let toml_str =
            toml::to_string_pretty(&config).expect("default config should serialize to TOML");
        assert!(!toml_str.is_empty());
        let _: config::AppConfig = toml::from_str(&toml_str).expect("serialized TOML should parse");
    }

    #[test]
    fn test_config_path_is_in_voom_dir() {
        let path = config::config_path();
        let dir = config::voom_config_dir();
        assert_eq!(path.parent().unwrap(), dir);
    }

    // ── resolve_toml_key ────────────────────────────────────

    #[test]
    fn test_resolve_top_level_key() {
        let table: toml::Table = toml::from_str("auth_token = \"secret\"").unwrap();
        let val = resolve_toml_key(&table, "auth_token").unwrap();
        assert_eq!(val.as_str().unwrap(), "secret");
    }

    #[test]
    fn test_resolve_nested_key() {
        let table: toml::Table = toml::from_str("[plugin.ffmpeg]\nhw_accel = \"nvenc\"").unwrap();
        let val = resolve_toml_key(&table, "plugin.ffmpeg.hw_accel").unwrap();
        assert_eq!(val.as_str().unwrap(), "nvenc");
    }

    #[test]
    fn test_resolve_missing_key_errors() {
        let table: toml::Table = toml::from_str("").unwrap();
        let err = resolve_toml_key(&table, "nonexistent").unwrap_err();
        assert!(err.to_string().contains("key not set"), "got: {err}");
    }

    #[test]
    fn test_resolve_returns_table() {
        let table: toml::Table =
            toml::from_str("[plugin.ffmpeg]\nhw_accel = \"nvenc\"\ngpu = \"0\"").unwrap();
        let val = resolve_toml_key(&table, "plugin.ffmpeg").unwrap();
        assert!(val.is_table());
    }

    // ── coerce_value ────────────────────────────────────────

    #[test]
    fn test_coerce_auto_bool() {
        assert_eq!(
            coerce_value("true", None).unwrap(),
            toml::Value::Boolean(true)
        );
        assert_eq!(
            coerce_value("false", None).unwrap(),
            toml::Value::Boolean(false)
        );
    }

    #[test]
    fn test_coerce_auto_int() {
        assert_eq!(coerce_value("42", None).unwrap(), toml::Value::Integer(42));
    }

    #[test]
    fn test_coerce_auto_float() {
        assert_eq!(
            coerce_value("3.14", None).unwrap(),
            toml::Value::Float(3.14)
        );
    }

    #[test]
    fn test_coerce_auto_string() {
        assert_eq!(
            coerce_value("hello", None).unwrap(),
            toml::Value::String("hello".into())
        );
    }

    #[test]
    fn test_coerce_matches_existing_string() {
        let existing = toml::Value::String("old".into());
        // "42" would auto-detect as int, but existing is string
        assert_eq!(
            coerce_value("42", Some(&existing)).unwrap(),
            toml::Value::String("42".into())
        );
    }

    #[test]
    fn test_coerce_matches_existing_bool() {
        let existing = toml::Value::Boolean(true);
        assert_eq!(
            coerce_value("false", Some(&existing)).unwrap(),
            toml::Value::Boolean(false)
        );
    }

    #[test]
    fn test_coerce_bool_type_mismatch_errors() {
        let existing = toml::Value::Boolean(true);
        assert!(coerce_value("notabool", Some(&existing)).is_err());
    }

    #[test]
    fn test_coerce_int_type_mismatch_errors() {
        let existing = toml::Value::Integer(1);
        assert!(coerce_value("abc", Some(&existing)).is_err());
    }

    #[test]
    fn test_coerce_rejects_array() {
        let existing = toml::Value::Array(vec![toml::Value::Integer(1)]);
        let err = coerce_value("x", Some(&existing)).unwrap_err();
        assert!(err.to_string().contains("cannot set array"), "got: {err}");
    }

    #[test]
    fn test_coerce_rejects_table() {
        let existing = toml::Value::Table(toml::Table::new());
        let err = coerce_value("x", Some(&existing)).unwrap_err();
        assert!(err.to_string().contains("cannot set a table"), "got: {err}");
    }

    // ── format_value ────────────────────────────────────────

    #[test]
    fn test_format_string() {
        let v = toml::Value::String("hello".into());
        assert_eq!(format_value(&v), "hello");
    }

    #[test]
    fn test_format_int() {
        let v = toml::Value::Integer(42);
        assert_eq!(format_value(&v), "42");
    }

    #[test]
    fn test_format_bool() {
        let v = toml::Value::Boolean(true);
        assert_eq!(format_value(&v), "true");
    }

    #[test]
    fn test_format_table_as_json() {
        let mut t = toml::Table::new();
        t.insert("key".into(), toml::Value::String("val".into()));
        let v = toml::Value::Table(t);
        let out = format_value(&v);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["key"], "val");
    }

    #[test]
    fn test_format_array_as_json() {
        let v = toml::Value::Array(vec![toml::Value::Integer(1), toml::Value::Integer(2)]);
        let out = format_value(&v);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed, serde_json::json!([1, 2]));
    }
}
