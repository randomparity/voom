//! Per-directory policy mapping: resolve which `.voom` policy applies to each file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::{AppConfig, MappingEntry};
use crate::policy_paths;

/// Standalone policy map file format (used with `--policy-map`).
#[derive(Debug, Deserialize)]
pub struct PolicyMapFile {
    pub default: Option<String>,
    #[serde(default)]
    pub mapping: Vec<MappingEntry>,
}

/// Resolves which compiled policy applies to a given file path.
pub struct PolicyResolver {
    root: PathBuf,
    /// Sorted by prefix length descending (longest prefix wins).
    mappings: Vec<ResolvedMapping>,
    default: DefaultAction,
    /// Deduplicated compiled policies: `(display_name, compiled)`.
    policies: Vec<(String, voom_dsl::CompiledPolicy)>,
}

/// What to do when no prefix matches.
enum DefaultAction {
    /// Use policy at this index in `self.policies`.
    Policy(usize),
    /// Skip all unmatched files.
    Skip,
    /// Error on unmatched files (no default specified).
    None,
}

struct ResolvedMapping {
    prefix: PathBuf,
    action: MappingAction,
}

enum MappingAction {
    /// Use policy at this index in `PolicyResolver::policies`.
    Policy(usize),
    Skip,
}

/// Resolve a policy path, checking `base_dir` first (if provided) before
/// falling back to the caller's normal policy search.
fn resolve_policy_in_context(
    name: &str,
    base_dir: Option<&Path>,
    fallback: &dyn Fn(&Path) -> PathBuf,
) -> PathBuf {
    if let Some(base) = base_dir {
        let candidate = base.join(name);
        if candidate.exists() {
            return candidate;
        }
    }
    fallback(Path::new(name))
}

/// Result of resolving a file against the policy map.
#[derive(Debug)]
pub enum PolicyMatch<'a> {
    /// Apply this compiled policy.
    Policy(&'a voom_dsl::CompiledPolicy, &'a str),
    /// Skip this file.
    Skip,
}

impl PolicyResolver {
    /// Wrap a single `--policy` file (backward-compatible path).
    pub fn from_single(compiled: voom_dsl::CompiledPolicy, root: &Path) -> Self {
        let name = compiled.name.clone();
        Self {
            root: root.to_path_buf(),
            mappings: Vec::new(),
            default: DefaultAction::Policy(0),
            policies: vec![(name, compiled)],
        }
    }

    /// Build from a standalone `--policy-map` TOML file.
    pub fn from_map_file(path: &Path, root: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read policy map: {}", path.display()))?;
        let map_file: PolicyMapFile = toml::from_str(&contents)
            .with_context(|| format!("failed to parse policy map: {}", path.display()))?;

        let base_dir = path.parent().map(Path::to_path_buf);
        Self::build(
            map_file.default.as_deref(),
            &map_file.mapping,
            root,
            base_dir.as_deref(),
            &policy_paths::resolve_policy_path,
        )
    }

    /// Build from `config.toml` fields.
    pub fn from_config(config: &AppConfig, root: &Path) -> Result<Self> {
        Self::build(
            config.default_policy.as_deref(),
            &config.policy_mapping,
            root,
            None,
            &policy_paths::resolve_policy_path,
        )
    }

    /// Shared builder: load + compile all referenced policies, build sorted mappings.
    ///
    /// When `base_dir` is `Some`, relative policy paths are first resolved
    /// against that directory (the map file's parent) before falling back
    /// to the supplied path resolver.
    fn build(
        default: Option<&str>,
        entries: &[MappingEntry],
        root: &Path,
        base_dir: Option<&Path>,
        fallback: &dyn Fn(&Path) -> PathBuf,
    ) -> Result<Self> {
        for entry in entries {
            entry.validate()?;
        }

        // Collect unique policy paths to compile each only once.
        let mut policy_paths: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        if let Some(d) = default {
            if d != "skip" {
                seen.insert(d.to_string());
                policy_paths.push(d.to_string());
            }
        }
        for entry in entries {
            if let Some(ref p) = entry.policy {
                if seen.insert(p.clone()) {
                    policy_paths.push(p.clone());
                }
            }
        }

        // Compile each unique policy.
        let mut policies: Vec<(String, voom_dsl::CompiledPolicy)> = Vec::new();
        let mut path_to_index: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        for policy_path_str in &policy_paths {
            let resolved = resolve_policy_in_context(policy_path_str, base_dir, fallback);
            let source = std::fs::read_to_string(&resolved)
                .with_context(|| format!("failed to read policy: {}", resolved.display()))?;
            let compiled =
                voom_dsl::compile_policy(&source).context("policy compilation failed")?;
            let idx = policies.len();
            policies.push((policy_path_str.clone(), compiled));
            path_to_index.insert(policy_path_str.clone(), idx);
        }

        // Build default action.
        let default_action = match default {
            Some("skip") => DefaultAction::Skip,
            Some(d) => DefaultAction::Policy(path_to_index[d]),
            None => DefaultAction::None,
        };

        // Build resolved mappings, sorted by prefix length descending.
        let mut mappings: Vec<ResolvedMapping> = entries
            .iter()
            .map(|entry| {
                let action = if entry.skip == Some(true) {
                    MappingAction::Skip
                } else {
                    let idx = path_to_index[entry.policy.as_ref().expect("validated")];
                    MappingAction::Policy(idx)
                };
                ResolvedMapping {
                    prefix: PathBuf::from(&entry.prefix),
                    action,
                }
            })
            .collect();
        mappings.sort_by(|a, b| b.prefix.as_os_str().len().cmp(&a.prefix.as_os_str().len()));

        Ok(Self {
            root: root.to_path_buf(),
            mappings,
            default: default_action,
            policies,
        })
    }

    /// Resolve which policy (or skip) applies to a file.
    pub fn resolve(&self, file_path: &Path) -> Result<PolicyMatch<'_>> {
        let relative = file_path.strip_prefix(&self.root).unwrap_or(file_path);

        // Longest prefix match (mappings are sorted by length desc).
        for mapping in &self.mappings {
            if relative.starts_with(&mapping.prefix) {
                return match &mapping.action {
                    MappingAction::Policy(idx) => {
                        let (name, compiled) = &self.policies[*idx];
                        Ok(PolicyMatch::Policy(compiled, name))
                    }
                    MappingAction::Skip => Ok(PolicyMatch::Skip),
                };
            }
        }

        // Fall back to default.
        match &self.default {
            DefaultAction::Policy(idx) => {
                let (name, compiled) = &self.policies[*idx];
                Ok(PolicyMatch::Policy(compiled, name))
            }
            DefaultAction::Skip => Ok(PolicyMatch::Skip),
            DefaultAction::None => {
                anyhow::bail!(
                    "no policy mapping matches {} and no default is configured",
                    relative.display()
                );
            }
        }
    }

    /// Union of all phase names across all loaded policies.
    pub fn all_phase_names(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut names = Vec::new();
        for (_, compiled) in &self.policies {
            for phase in &compiled.phase_order {
                if seen.insert(phase.clone()) {
                    names.push(phase.clone());
                }
            }
        }
        names
    }

    /// Human-readable summary for header output.
    pub fn summary(&self) -> String {
        if self.is_single_policy() {
            let (name, _) = &self.policies[0];
            return name.clone();
        }
        let policy_count = self.policies.len();
        let mapping_count = self.mappings.len();
        format!("{policy_count} policies, {mapping_count} prefix mappings")
    }

    /// True if this wraps a single `--policy` (no map).
    pub fn is_single_policy(&self) -> bool {
        self.mappings.is_empty() && self.policies.len() == 1
    }

    /// Access the list of loaded policies (for validation, etc.).
    pub fn policies(&self) -> &[(String, voom_dsl::CompiledPolicy)] {
        &self.policies
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY_A: &str = r#"
policy "policy-a" {
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

    const POLICY_B: &str = r#"
policy "policy-b" {
  config {
    languages audio: [eng, jpn]
    languages subtitle: [eng, jpn]
  }
  phase normalize {
    keep audio where lang in [eng, jpn]
    keep subtitles where lang in [eng, jpn]
  }
  phase cleanup {
    remove subtitles where commentary
  }
}
"#;

    /// Helper: create temp policy files and return (dir, `path_a`, `path_b`).
    fn setup_policies() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let pa = dir.path().join("a.voom");
        let pb = dir.path().join("b.voom");
        std::fs::write(&pa, POLICY_A).expect("write a");
        std::fs::write(&pb, POLICY_B).expect("write b");
        (dir, pa, pb)
    }

    // ── PolicyMapFile TOML parsing ──────────────────────────

    #[test]
    fn parse_map_file_valid() {
        let toml_str = r#"
default = "standard.voom"

[[mapping]]
prefix = "best-hd"
policy = "high-quality.voom"

[[mapping]]
prefix = "test-bad"
skip = true
"#;
        let map: PolicyMapFile = toml::from_str(toml_str).expect("parse");
        assert_eq!(map.default.as_deref(), Some("standard.voom"));
        assert_eq!(map.mapping.len(), 2);
        assert_eq!(map.mapping[0].prefix, "best-hd");
        assert_eq!(map.mapping[0].policy.as_deref(), Some("high-quality.voom"));
        assert_eq!(map.mapping[1].skip, Some(true));
    }

    #[test]
    fn parse_map_file_no_default() {
        let toml_str = r#"
[[mapping]]
prefix = "foo"
policy = "bar.voom"
"#;
        let map: PolicyMapFile = toml::from_str(toml_str).expect("parse");
        assert!(map.default.is_none());
        assert_eq!(map.mapping.len(), 1);
    }

    #[test]
    fn parse_map_file_empty() {
        let map: PolicyMapFile = toml::from_str("").expect("parse");
        assert!(map.default.is_none());
        assert!(map.mapping.is_empty());
    }

    // ── MappingEntry validation ─────────────────────────────

    #[test]
    fn validate_entry_policy_only() {
        let entry = MappingEntry {
            prefix: "foo".into(),
            policy: Some("bar.voom".into()),
            skip: None,
        };
        assert!(entry.validate().is_ok());
    }

    #[test]
    fn validate_entry_skip_only() {
        let entry = MappingEntry {
            prefix: "foo".into(),
            policy: None,
            skip: Some(true),
        };
        assert!(entry.validate().is_ok());
    }

    #[test]
    fn validate_entry_both_rejected() {
        let entry = MappingEntry {
            prefix: "foo".into(),
            policy: Some("bar.voom".into()),
            skip: Some(true),
        };
        let err = entry.validate().unwrap_err();
        assert!(
            err.to_string().contains("both"),
            "error should mention both: {err}"
        );
    }

    #[test]
    fn validate_entry_neither_rejected() {
        let entry = MappingEntry {
            prefix: "foo".into(),
            policy: None,
            skip: None,
        };
        let err = entry.validate().unwrap_err();
        assert!(
            err.to_string().contains("must have"),
            "error should explain requirement: {err}"
        );
    }

    #[test]
    fn validate_entry_skip_false_rejected() {
        let entry = MappingEntry {
            prefix: "foo".into(),
            policy: None,
            skip: Some(false),
        };
        assert!(entry.validate().is_err());
    }

    // ── PolicyResolver::from_single ─────────────────────────

    #[test]
    fn from_single_backward_compat() {
        let compiled = voom_dsl::compile_policy(POLICY_A).expect("compile");
        let root = PathBuf::from("/media");
        let resolver = PolicyResolver::from_single(compiled, &root);

        assert!(resolver.is_single_policy());
        assert_eq!(resolver.summary(), "policy-a");

        let m = resolver
            .resolve(Path::new("/media/some/file.mkv"))
            .expect("resolve");
        assert!(matches!(m, PolicyMatch::Policy(_, "policy-a")));
    }

    // ── PolicyResolver::resolve longest prefix ──────────────

    #[test]
    fn resolve_longest_prefix_wins() {
        let (dir, pa, pb) = setup_policies();
        let root = dir.path().to_path_buf();

        let map_toml = format!(
            r#"
default = "{pa}"

[[mapping]]
prefix = "best"
policy = "{pa}"

[[mapping]]
prefix = "best/classics"
policy = "{pb}"
"#,
            pa = pa.display(),
            pb = pb.display()
        );
        let map_file = root.join("map.toml");
        std::fs::write(&map_file, map_toml).expect("write map");

        let resolver = PolicyResolver::from_map_file(&map_file, &root).expect("build");

        // "best/classics/foo.mkv" should match "best/classics" (longer) -> policy-b
        let m = resolver
            .resolve(&root.join("best/classics/foo.mkv"))
            .expect("resolve");
        match m {
            PolicyMatch::Policy(_, name) => assert_eq!(name, pb.display().to_string()),
            PolicyMatch::Skip => panic!("expected policy, got skip"),
        }

        // "best/other.mkv" should match "best" -> policy-a
        let m = resolver
            .resolve(&root.join("best/other.mkv"))
            .expect("resolve");
        match m {
            PolicyMatch::Policy(_, name) => assert_eq!(name, pa.display().to_string()),
            PolicyMatch::Skip => panic!("expected policy, got skip"),
        }
    }

    // ── PolicyResolver default and skip ─────────────────────

    #[test]
    fn resolve_uses_default_for_unmatched() {
        let (dir, pa, _pb) = setup_policies();
        let root = dir.path().to_path_buf();

        let map_toml = format!(
            r#"
default = "{pa}"

[[mapping]]
prefix = "special"
skip = true
"#,
            pa = pa.display()
        );
        let map_file = root.join("map.toml");
        std::fs::write(&map_file, map_toml).expect("write");

        let resolver = PolicyResolver::from_map_file(&map_file, &root).expect("build");

        // Unmatched -> default policy-a
        let m = resolver
            .resolve(&root.join("random/file.mkv"))
            .expect("resolve");
        assert!(matches!(m, PolicyMatch::Policy(..)));

        // "special/file.mkv" -> skip
        let m = resolver
            .resolve(&root.join("special/file.mkv"))
            .expect("resolve");
        assert!(matches!(m, PolicyMatch::Skip));
    }

    #[test]
    fn resolve_skip_default() {
        let (dir, pa, _pb) = setup_policies();
        let root = dir.path().to_path_buf();

        let map_toml = format!(
            r#"
default = "skip"

[[mapping]]
prefix = "process-me"
policy = "{pa}"
"#,
            pa = pa.display()
        );
        let map_file = root.join("map.toml");
        std::fs::write(&map_file, map_toml).expect("write");

        let resolver = PolicyResolver::from_map_file(&map_file, &root).expect("build");

        let m = resolver
            .resolve(&root.join("unmatched/file.mkv"))
            .expect("resolve");
        assert!(matches!(m, PolicyMatch::Skip));
    }

    #[test]
    fn resolve_no_default_errors_on_unmatched() {
        let (dir, pa, _pb) = setup_policies();
        let root = dir.path().to_path_buf();

        let map_toml = format!(
            r#"
[[mapping]]
prefix = "only-this"
policy = "{pa}"
"#,
            pa = pa.display()
        );
        let map_file = root.join("map.toml");
        std::fs::write(&map_file, map_toml).expect("write");

        let resolver = PolicyResolver::from_map_file(&map_file, &root).expect("build");

        let result = resolver.resolve(&root.join("other/file.mkv"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no policy mapping"),
            "should say no mapping: {err}"
        );
    }

    // ── all_phase_names / summary ───────────────────────────

    #[test]
    fn all_phase_names_unions_policies() {
        let (dir, pa, pb) = setup_policies();
        let root = dir.path().to_path_buf();

        let map_toml = format!(
            r#"
default = "{pa}"

[[mapping]]
prefix = "hq"
policy = "{pb}"
"#,
            pa = pa.display(),
            pb = pb.display()
        );
        let map_file = root.join("map.toml");
        std::fs::write(&map_file, map_toml).expect("write");

        let resolver = PolicyResolver::from_map_file(&map_file, &root).expect("build");

        let names = resolver.all_phase_names();
        assert!(names.contains(&"normalize".to_string()));
        assert!(names.contains(&"cleanup".to_string()));
    }

    #[test]
    fn summary_multi_policy() {
        let (dir, pa, pb) = setup_policies();
        let root = dir.path().to_path_buf();

        let map_toml = format!(
            r#"
default = "{pa}"

[[mapping]]
prefix = "hq"
policy = "{pb}"
"#,
            pa = pa.display(),
            pb = pb.display()
        );
        let map_file = root.join("map.toml");
        std::fs::write(&map_file, map_toml).expect("write");

        let resolver = PolicyResolver::from_map_file(&map_file, &root).expect("build");

        assert!(!resolver.is_single_policy());
        let summary = resolver.summary();
        assert!(summary.contains("2 policies"), "got: {summary}");
        assert!(summary.contains("1 prefix"), "got: {summary}");
    }

    // ── Validation rejects bad entries ──────────────────────

    #[test]
    fn build_rejects_both_policy_and_skip() {
        let (dir, pa, _pb) = setup_policies();
        let root = dir.path().to_path_buf();

        let map_toml = format!(
            r#"
[[mapping]]
prefix = "bad"
policy = "{pa}"
skip = true
"#,
            pa = pa.display()
        );
        let map_file = root.join("map.toml");
        std::fs::write(&map_file, map_toml).expect("write");

        let result = PolicyResolver::from_map_file(&map_file, &root);
        assert!(result.is_err());
    }

    // ── from_config ─────────────────────────────────────────

    #[test]
    fn from_config_with_mappings() {
        let (dir, pa, pb) = setup_policies();
        let root = dir.path().to_path_buf();

        let config = AppConfig {
            default_policy: Some(pa.display().to_string()),
            policy_mapping: vec![MappingEntry {
                prefix: "hq".into(),
                policy: Some(pb.display().to_string()),
                skip: None,
            }],
            ..Default::default()
        };

        let resolver = PolicyResolver::from_config(&config, &root).expect("build");
        assert!(!resolver.is_single_policy());

        let m = resolver
            .resolve(&root.join("hq/file.mkv"))
            .expect("resolve");
        assert!(matches!(m, PolicyMatch::Policy(..)));
    }

    // ── Fix: relative policy paths resolve from map file dir ───

    #[test]
    fn map_file_resolves_relative_paths_from_its_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let subdir = dir.path().join("configs");
        std::fs::create_dir_all(&subdir).expect("mkdir");

        // Policy lives next to the map file in configs/
        let policy_path = subdir.join("local.voom");
        std::fs::write(&policy_path, POLICY_A).expect("write policy");

        // Map file references "local.voom" (relative)
        let map_toml = r#"
default = "local.voom"
"#;
        let map_file = subdir.join("map.toml");
        std::fs::write(&map_file, map_toml).expect("write map");

        // Root is the parent dir, not configs/
        let root = dir.path().to_path_buf();
        let resolver = PolicyResolver::from_map_file(&map_file, &root).expect("build");

        let m = resolver
            .resolve(&root.join("any/file.mkv"))
            .expect("resolve");
        assert!(matches!(m, PolicyMatch::Policy(_, "local.voom")));
    }

    #[test]
    fn map_file_falls_back_when_not_next_to_map() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Policy lives at the root, NOT next to map file
        let policy_path = dir.path().join("global.voom");
        std::fs::write(&policy_path, POLICY_A).expect("write policy");

        let subdir = dir.path().join("configs");
        std::fs::create_dir_all(&subdir).expect("mkdir");

        // Map file in configs/ references an absolute path
        let map_toml = format!("default = \"{}\"\n", policy_path.display());
        let map_file = subdir.join("map.toml");
        std::fs::write(&map_file, map_toml).expect("write map");

        let root = dir.path().to_path_buf();
        let resolver = PolicyResolver::from_map_file(&map_file, &root).expect("build");

        let m = resolver
            .resolve(&root.join("any/file.mkv"))
            .expect("resolve");
        assert!(matches!(m, PolicyMatch::Policy(..)));
    }

    // ── Fix: single-file target — root must be directory, not file ──

    #[test]
    fn file_as_root_breaks_prefix_matching() {
        // Demonstrates the bug: when root equals the file path,
        // strip_prefix yields "" and no prefix can ever match.
        let (dir, _pa, pb) = setup_policies();
        let media_dir = dir.path().join("movies");
        std::fs::create_dir_all(&media_dir).expect("mkdir");
        let file_path = media_dir.join("film.mkv");
        std::fs::write(&file_path, "fake").expect("write");

        let map_toml = format!(
            r#"
[[mapping]]
prefix = "movies"
policy = "{pb}"
"#,
            pb = pb.display()
        );
        let map_file = dir.path().join("map.toml");
        std::fs::write(&map_file, map_toml).expect("write map");

        // Bug: using the file itself as root — strip_prefix yields ""
        let resolver = PolicyResolver::from_map_file(&map_file, &file_path).expect("build");
        let result = resolver.resolve(&file_path);
        assert!(
            result.is_err(),
            "file-as-root should fail prefix matching (no default configured)"
        );
    }

    #[test]
    fn parent_dir_as_root_fixes_prefix_matching() {
        // The fix: use the file's parent directory as root so
        // strip_prefix produces "movies/film.mkv" and prefixes match.
        let (dir, _pa, pb) = setup_policies();
        let media_dir = dir.path().join("movies");
        std::fs::create_dir_all(&media_dir).expect("mkdir");
        let file_path = media_dir.join("film.mkv");
        std::fs::write(&file_path, "fake").expect("write");

        let map_toml = format!(
            r#"
[[mapping]]
prefix = "movies"
policy = "{pb}"
"#,
            pb = pb.display()
        );
        let map_file = dir.path().join("map.toml");
        std::fs::write(&map_file, map_toml).expect("write map");

        // Fix: use the parent directory as root
        let root = dir.path().to_path_buf();
        let resolver = PolicyResolver::from_map_file(&map_file, &root).expect("build");
        let m = resolver.resolve(&file_path).expect("resolve");
        match m {
            PolicyMatch::Policy(_, name) => {
                assert_eq!(name, pb.display().to_string());
            }
            PolicyMatch::Skip => panic!("expected policy, got skip"),
        }
    }
}
