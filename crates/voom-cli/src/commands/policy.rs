use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use console::style;
use serde::Serialize;
use serde_json::Value;
use voom_dsl::compiled::{
    CompiledMetadata, CompiledPhase, CompiledPhaseComposition, PhaseCompositionKind,
};
use voom_policy_testing::{
    CapabilityFixture, Fixture, SnapshotOutcome, TestSuite, assert_snapshot_file,
};

use crate::cli::{OutputFormat, PolicyCommands, PolicyFixtureCommands};
use crate::output;

pub async fn run(cmd: PolicyCommands) -> Result<()> {
    match cmd {
        PolicyCommands::List { format } => list(format),
        PolicyCommands::Validate { file, format } => validate(&file, format),
        PolicyCommands::Show { file, format } => show(&file, format),
        PolicyCommands::Describe { file, format } => describe(&file, format),
        PolicyCommands::Format { file } => format(&file),
        PolicyCommands::Diff {
            a,
            b,
            fixture,
            format,
        } => diff(&a, &b, fixture.as_deref(), format),
        PolicyCommands::Fixture { command } => fixture(command).await,
        PolicyCommands::Test {
            paths,
            policy,
            update,
            format,
        } => test(&paths, policy.as_deref(), update, format),
    }
}

async fn fixture(cmd: PolicyFixtureCommands) -> Result<()> {
    match cmd {
        PolicyFixtureCommands::Extract { path } => extract_fixture(&path).await,
    }
}

async fn extract_fixture(path: &Path) -> Result<()> {
    let config = crate::config::load_config()?;
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("failed to stat media file {}", path.display()))?;
    if !metadata.is_file() {
        bail!("fixture source is not a file: {}", path.display());
    }

    let kernel = Arc::new(voom_kernel::Kernel::new());
    let media = crate::introspect::introspect_file_no_dispatch(
        path.to_path_buf(),
        metadata.len(),
        None,
        &kernel,
        config.ffprobe_path(),
        config.animation_detection_mode(),
    )
    .await
    .with_context(|| format!("failed to introspect {}", path.display()))?;
    let fixture = fixture_from_media(media)?;
    println!("{}", serde_json::to_string_pretty(&fixture)?);
    Ok(())
}

fn fixture_from_media(media: voom_domain::media::MediaFile) -> Result<Fixture> {
    let file_name = media
        .path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("media path has no filename: {}", media.path.display()))?;
    Ok(Fixture {
        path: PathBuf::from(file_name),
        container: media.container,
        duration: media.duration,
        size: media.size,
        tracks: media.tracks,
        capabilities: None,
    })
}

fn list(format: OutputFormat) -> Result<()> {
    // Scan standard policy directories
    let config_dir = crate::config::policies_dir();

    if !config_dir.exists() {
        if matches!(format, OutputFormat::Json) {
            output::print_json(&Vec::<serde_json::Value>::new())?;
            return Ok(());
        }
        println!("{}", style("No policies directory found.").dim());
        println!("Create policies in: {}", style(config_dir.display()).cyan());
        return Ok(());
    }

    let mut policies = Vec::new();
    for entry in std::fs::read_dir(&config_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "voom") {
            let name = path
                .file_stem()
                .expect("file has .voom extension so stem exists")
                .to_string_lossy();
            match voom_dsl::compile_policy_file(&path) {
                Ok(policy) => {
                    let policy_name = policy.name.clone();
                    let phase_count = policy.phases.len();
                    policies.push(serde_json::json!({
                        "name": policy_name,
                        "path": path,
                        "valid": true,
                        "phase_count": phase_count,
                        "phase_order": policy.phase_order,
                    }));
                    if !matches!(format, OutputFormat::Json) {
                        println!(
                            "  {} {} ({} phases)",
                            style("OK").green(),
                            style(&policy_name).bold(),
                            phase_count
                        );
                    }
                }
                Err(e) => {
                    let error = e.to_string();
                    let policy_name = name.to_string();
                    policies.push(serde_json::json!({
                        "name": policy_name,
                        "path": path,
                        "valid": false,
                        "error": error,
                    }));
                    if !matches!(format, OutputFormat::Json) {
                        println!(
                            "  {} {} — {error}",
                            style("ERR").red(),
                            style(&policy_name).bold()
                        );
                    }
                }
            }
        }
    }

    if matches!(format, OutputFormat::Json) {
        output::print_json(&policies)?;
    } else if policies.is_empty() {
        println!("{}", style("No .voom policy files found.").dim());
    }

    Ok(())
}

fn validate(file: &std::path::Path, format: OutputFormat) -> Result<()> {
    // If the file has a .toml extension, treat it as a policy map.
    if file.extension().is_some_and(|e| e == "toml") {
        return validate_policy_map(file, format);
    }

    let file = crate::config::resolve_policy_path(file);

    match voom_dsl::compile_policy_file(&file) {
        Ok(policy) => {
            if matches!(format, OutputFormat::Json) {
                output::print_json(&serde_json::json!({
                    "valid": true,
                    "policy": policy.name,
                    "phase_count": policy.phases.len(),
                    "phase_order": policy.phase_order,
                }))?;
            } else {
                println!(
                    "{} Policy \"{}\" is valid ({} phases, {} phase order: [{}])",
                    style("OK").bold().green(),
                    policy.name,
                    policy.phases.len(),
                    policy.phase_order.len(),
                    policy.phase_order.join(", ")
                );
            }
        }
        Err(e) => {
            anyhow::bail!("Policy validation failed: {e}");
        }
    }

    Ok(())
}

/// Validate a `.toml` policy map file and all policies it references.
fn validate_policy_map(file: &std::path::Path, format: OutputFormat) -> Result<()> {
    use crate::policy_map::PolicyResolver;

    // Use a dummy root — we only care about compilation, not prefix matching.
    let root = std::path::PathBuf::from(".");
    let resolver = PolicyResolver::from_map_file(file, &root)
        .with_context(|| format!("Policy map validation failed: {}", file.display()))?;

    let policies = resolver.policies();
    if matches!(format, OutputFormat::Json) {
        let policies: Vec<serde_json::Value> = policies
            .iter()
            .map(|(name, compiled)| {
                serde_json::json!({
                    "name": name,
                    "policy": compiled.name,
                    "phase_count": compiled.phases.len(),
                    "phase_order": compiled.phase_order,
                })
            })
            .collect();
        output::print_json(&serde_json::json!({
            "valid": true,
            "policy_map": file,
            "policy_count": policies.len(),
            "policies": policies,
        }))?;
        return Ok(());
    }

    println!(
        "{} Policy map \"{}\" is valid ({} policies)",
        style("OK").bold().green(),
        file.display(),
        policies.len(),
    );
    for (name, compiled) in policies {
        println!(
            "  {} {} — \"{}\" ({} phases: [{}])",
            style("OK").green(),
            name,
            compiled.name,
            compiled.phases.len(),
            compiled.phase_order.join(", ")
        );
    }
    Ok(())
}

fn show(file: &std::path::Path, format: OutputFormat) -> Result<()> {
    let file = crate::config::resolve_policy_path(file);

    let compiled = compile_policy_file(&file, "policy")?;
    if matches!(format, OutputFormat::Json) {
        output::print_json(&compiled)?;
        return Ok(());
    }

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

fn describe(file: &std::path::Path, format: OutputFormat) -> Result<()> {
    let file = crate::config::resolve_policy_path(file);
    let compiled = compile_policy_file(&file, "policy")?;
    let output = DescribeOutput::from_policy(&compiled);

    if matches!(format, OutputFormat::Json) {
        output::print_json(&output)?;
        return Ok(());
    }

    print_human_describe(&output, &compiled.metadata.version);
    Ok(())
}

fn format(file: &std::path::Path) -> Result<()> {
    let file = crate::config::resolve_policy_path(file);
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

fn diff(
    a: &std::path::Path,
    b: &std::path::Path,
    fixture: Option<&std::path::Path>,
    format: OutputFormat,
) -> Result<()> {
    let a_path = crate::config::resolve_policy_path(a);
    let b_path = crate::config::resolve_policy_path(b);

    let a_compiled = compile_policy_file(&a_path, "first")?;
    let b_compiled = compile_policy_file(&b_path, "second")?;

    let (a_json, b_json) = if let Some(fixture) = fixture {
        fixture_plan_json(&a_compiled, &b_compiled, fixture)?
    } else {
        let mut a_json =
            serde_json::to_value(&a_compiled).context("failed to serialize first policy")?;
        let mut b_json =
            serde_json::to_value(&b_compiled).context("failed to serialize second policy")?;
        strip_policy_volatiles(&mut a_json);
        strip_policy_volatiles(&mut b_json);
        (a_json, b_json)
    };

    let mut lines = Vec::new();
    diff_values("", &a_json, &b_json, &mut lines);

    if matches!(format, OutputFormat::Json) {
        let differences: Vec<String> = lines.iter().map(render_diff_line).collect();
        output::print_json(&serde_json::json!({
            "identical": lines.is_empty(),
            "differences": differences,
        }))?;
        if fixture.is_some() && !lines.is_empty() {
            anyhow::bail!("fixture plan diff found differences");
        }
        return Ok(());
    }

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

    if fixture.is_some() {
        anyhow::bail!("fixture plan diff found differences");
    }

    Ok(())
}

fn test(
    paths: &[PathBuf],
    policy_override: Option<&Path>,
    update: bool,
    format: OutputFormat,
) -> Result<()> {
    let suites = discover_test_suites(paths)?;
    if suites.is_empty() {
        bail!("no *.test.json files found");
    }

    let mut cases = Vec::new();
    let mut snapshots_updated = 0;
    for suite_path in suites {
        run_test_suite(&suite_path, policy_override, update, &mut cases).map(|updated| {
            snapshots_updated += updated;
        })?;
    }

    let output = TestOutput::from_cases(cases, snapshots_updated);
    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        print_human_test_output(&output);
    }
    if output.summary.failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn discover_test_suites(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut suites = Vec::new();
    for path in paths {
        if path.is_file() {
            suites.push(path.clone());
            continue;
        }
        if path.is_dir() {
            collect_test_suites(path, &mut suites)?;
            continue;
        }
        bail!("test path does not exist: {}", path.display());
    }
    suites.sort();
    Ok(suites)
}

fn collect_test_suites(dir: &Path, suites: &mut Vec<PathBuf>) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_test_suites(&path, suites)?;
        } else if is_test_suite_file(&path) {
            suites.push(path);
        }
    }
    Ok(())
}

fn is_test_suite_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(".test.json"))
}

fn run_test_suite(
    suite_path: &Path,
    policy_override: Option<&Path>,
    update: bool,
    results: &mut Vec<TestCaseOutput>,
) -> Result<usize> {
    let suite = TestSuite::load(suite_path)?;
    let suite_dir = suite_path.parent().unwrap_or_else(|| Path::new("."));
    let policy_path = policy_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| resolve_relative(suite_dir, &suite.policy));
    let policy = voom_dsl::compile_policy_file(&policy_path)
        .with_context(|| format!("failed to compile policy {}", policy_path.display()))?;

    let mut snapshots_updated = 0;
    for case in &suite.cases {
        let fixture_path = resolve_relative(suite_dir, &case.fixture);
        let fixture = Fixture::load(&fixture_path)?;
        let capabilities = capabilities_for_case(case.capabilities.as_ref(), &fixture);
        let evaluation = voom_policy_evaluator::evaluate_with_capabilities(
            &policy,
            &fixture.to_media_file(),
            &capabilities,
        );
        let mut failures = case
            .expect
            .check(&evaluation.plans)
            .err()
            .map_or_else(Vec::new, |failure| vec![failure.to_string()]);
        if let Some(snapshot) = &case.snapshot {
            let snapshot_path = resolve_relative(suite_dir, snapshot);
            match assert_snapshot_file(&evaluation.plans, &snapshot_path, update) {
                Ok(SnapshotOutcome::Updated) => {
                    snapshots_updated += 1;
                }
                Ok(SnapshotOutcome::Matched) => {}
                Err(failure) => failures.push(failure.to_string()),
            }
        }
        results.push(TestCaseOutput {
            name: case.name.clone(),
            policy: policy_path.display().to_string(),
            fixture: fixture_path.display().to_string(),
            status: if failures.is_empty() {
                TestStatus::Pass
            } else {
                TestStatus::Fail
            },
            failures,
        });
    }
    Ok(snapshots_updated)
}

fn resolve_relative(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn capabilities_for_case(
    case: Option<&CapabilityFixture>,
    fixture: &Fixture,
) -> voom_domain::capability_map::CapabilityMap {
    case.map_or_else(
        || fixture.capabilities_or_default(),
        CapabilityFixture::to_capability_map,
    )
}

fn print_human_test_output(output: &TestOutput) {
    for case in &output.cases {
        match case.status {
            TestStatus::Pass => {
                println!(
                    "{} {} ({})",
                    style("OK").green(),
                    case.name,
                    style(&case.fixture).dim()
                );
            }
            TestStatus::Fail => {
                println!(
                    "{} {} ({})",
                    style("FAIL").red(),
                    case.name,
                    style(&case.fixture).dim()
                );
                for failure in &case.failures {
                    println!("  {failure}");
                }
            }
        }
    }
    println!(
        "{} passed, {} failed, {} total",
        output.summary.passed, output.summary.failed, output.summary.total
    );
    match output.summary.snapshots_updated {
        0 => println!("no snapshots regenerated"),
        1 => println!("updated 1 snapshot"),
        count => println!("updated {count} snapshots"),
    }
}

#[derive(Debug, Serialize)]
struct DescribeOutput {
    policy: String,
    extends_chain: Vec<String>,
    metadata: CompiledMetadata,
    phase_order: Vec<String>,
    phases: Vec<DescribePhase>,
}

impl DescribeOutput {
    fn from_policy(policy: &voom_dsl::CompiledPolicy) -> Self {
        Self {
            policy: policy.name.clone(),
            extends_chain: policy.metadata.extends_chain.clone(),
            metadata: policy.metadata.clone(),
            phase_order: policy.phase_order.clone(),
            phases: ordered_describe_phases(policy),
        }
    }
}

#[derive(Debug, Serialize)]
struct DescribePhase {
    name: String,
    composition: CompiledPhaseComposition,
}

impl DescribePhase {
    fn from_phase(phase: &CompiledPhase) -> Self {
        Self {
            name: phase.name.clone(),
            composition: phase.composition.clone(),
        }
    }
}

fn ordered_describe_phases(policy: &voom_dsl::CompiledPolicy) -> Vec<DescribePhase> {
    let mut phases = Vec::with_capacity(policy.phases.len());
    let mut used = vec![false; policy.phases.len()];

    for phase_name in &policy.phase_order {
        if let Some((index, phase)) = policy
            .phases
            .iter()
            .enumerate()
            .find(|(index, phase)| !used[*index] && phase.name == *phase_name)
        {
            phases.push(DescribePhase::from_phase(phase));
            used[index] = true;
        }
    }

    for (index, phase) in policy.phases.iter().enumerate() {
        if !used[index] {
            phases.push(DescribePhase::from_phase(phase));
        }
    }

    phases
}

fn print_human_describe(output: &DescribeOutput, version: &Option<String>) {
    print!("{}", render_human_describe(output, version));
}

fn render_human_describe(output: &DescribeOutput, version: &Option<String>) -> String {
    let mut rendered = String::new();
    rendered.push_str(&format!("Policy: {}\n", output.policy));
    rendered.push_str(&format!(
        "Extends: {}\n",
        format_extends_chain(&output.extends_chain)
    ));
    if let Some(version) = version {
        rendered.push_str(&format!("Version: {version}\n"));
    }
    rendered.push_str(&format!(
        "Effective phases: {}\n",
        output.phase_order.join(", ")
    ));

    let width = output
        .phases
        .iter()
        .map(|phase| phase.name.len())
        .max()
        .unwrap_or(0);
    for phase in &output.phases {
        rendered.push_str(&format!(
            "  {:width$}  {}\n",
            phase.name,
            format_composition(&phase.composition),
            width = width,
        ));
    }
    rendered
}

fn format_extends_chain(extends_chain: &[String]) -> String {
    if extends_chain.is_empty() {
        "none".to_string()
    } else {
        extends_chain.join(" -> ")
    }
}

fn format_composition(composition: &CompiledPhaseComposition) -> String {
    match composition.kind {
        PhaseCompositionKind::Local => "local".to_string(),
        PhaseCompositionKind::Inherited => {
            let source = composition.source.as_deref().unwrap_or("unknown");
            format!("inherited from {source}")
        }
        PhaseCompositionKind::Extended => {
            let source = composition.source.as_deref().unwrap_or("unknown");
            let count = composition.added_operations;
            let operation = if count == 1 {
                "operation"
            } else {
                "operations"
            };
            format!("extended from {source} ({count} {operation} added)")
        }
        PhaseCompositionKind::Overridden => {
            let source = composition.source.as_deref().unwrap_or("unknown");
            format!("overridden by {source}")
        }
    }
}

#[derive(Debug, Serialize)]
struct TestOutput {
    cases: Vec<TestCaseOutput>,
    summary: TestSummary,
}

impl TestOutput {
    fn from_cases(cases: Vec<TestCaseOutput>, snapshots_updated: usize) -> Self {
        let passed = cases
            .iter()
            .filter(|case| case.status == TestStatus::Pass)
            .count();
        let failed = cases.len() - passed;
        let total = cases.len();
        Self {
            cases,
            summary: TestSummary {
                passed,
                failed,
                total,
                snapshots_updated,
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct TestCaseOutput {
    name: String,
    policy: String,
    fixture: String,
    status: TestStatus,
    failures: Vec<String>,
}

#[derive(Debug, Serialize)]
struct TestSummary {
    passed: usize,
    failed: usize,
    total: usize,
    snapshots_updated: usize,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum TestStatus {
    Pass,
    Fail,
}

fn compile_policy_file(path: &std::path::Path, label: &str) -> Result<voom_dsl::CompiledPolicy> {
    voom_dsl::compile_policy_file(path)
        .with_context(|| format!("failed to compile {label} policy {}", path.display()))
}

fn fixture_plan_json(
    a: &voom_dsl::CompiledPolicy,
    b: &voom_dsl::CompiledPolicy,
    fixture_path: &std::path::Path,
) -> Result<(Value, Value)> {
    let fixture = Fixture::load(fixture_path)
        .with_context(|| format!("failed to load fixture: {}", fixture_path.display()))?;
    let file = fixture.to_media_file();
    let capabilities = fixture.capabilities_or_default();
    let a_plans = voom_policy_evaluator::evaluate_with_capabilities(a, &file, &capabilities).plans;
    let b_plans = voom_policy_evaluator::evaluate_with_capabilities(b, &file, &capabilities).plans;
    let mut a_json = serde_json::to_value(a_plans).context("failed to serialize first plans")?;
    let mut b_json = serde_json::to_value(b_plans).context("failed to serialize second plans")?;
    strip_plan_volatiles(&mut a_json);
    strip_plan_volatiles(&mut b_json);
    Ok((a_json, b_json))
}

fn strip_policy_volatiles(value: &mut Value) {
    if let Value::Object(map) = value {
        map.remove("source_hash");
    }
}

fn strip_plan_volatiles(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in [
                "evaluated_at",
                "id",
                "introspected_at",
                "policy_hash",
                "session_id",
            ] {
                map.remove(key);
            }
            for child in map.values_mut() {
                strip_plan_volatiles(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_plan_volatiles(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
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
            if diff_array_key(a_arr).is_some() || diff_array_key(b_arr).is_some() =>
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

/// Return the field used to match arrays of objects across policy and plan diffs.
fn diff_array_key(arr: &[Value]) -> Option<&'static str> {
    let first = arr.first()?.as_object()?;
    if first.contains_key("name") {
        Some("name")
    } else if first.contains_key("phase_name") {
        Some("phase_name")
    } else {
        None
    }
}

/// Diff arrays of named objects by matching on the "name" field.
fn diff_named_arrays(path: &str, a_arr: &[Value], b_arr: &[Value], lines: &mut Vec<DiffLine>) {
    let key = diff_array_key(a_arr)
        .or_else(|| diff_array_key(b_arr))
        .unwrap_or("name");
    let a_names: Vec<&str> = a_arr.iter().filter_map(|v| v.get(key)?.as_str()).collect();
    let b_names: Vec<&str> = b_arr.iter().filter_map(|v| v.get(key)?.as_str()).collect();

    let a_by_name: std::collections::HashMap<&str, &Value> = a_arr
        .iter()
        .filter_map(|v| Some((v.get(key)?.as_str()?, v)))
        .collect();
    let b_by_name: std::collections::HashMap<&str, &Value> = b_arr
        .iter()
        .filter_map(|v| Some((v.get(key)?.as_str()?, v)))
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

fn render_diff_line(line: &DiffLine) -> String {
    match line {
        DiffLine::Added { path, value } => {
            format!("+ {}: {}", strip_section(path), format_value(value))
        }
        DiffLine::Removed { path, value } => {
            format!("- {}: {}", strip_section(path), format_value(value))
        }
        DiffLine::Changed { path, old, new } => {
            format!(
                "~ {}: {} -> {}",
                strip_section(path),
                format_value(old),
                format_value(new)
            )
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
    use voom_domain::media::{Container, Track, TrackType};
    use voom_policy_testing::Fixture;

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
        let result = validate(&file, OutputFormat::Table);
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
        let result = validate(
            std::path::Path::new("/nonexistent/test.voom"),
            OutputFormat::Table,
        );
        assert!(result.is_err());
    }

    #[test]
    fn format_valid_policy_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.voom");
        std::fs::write(&file, MINIMAL_POLICY).unwrap();

        let result = format(&file);
        assert!(result.is_ok());

        // Verify the file was rewritten
        let formatted = std::fs::read_to_string(&file).unwrap();
        assert!(formatted.contains("policy"));
        assert!(formatted.contains("test-policy"));
    }

    #[test]
    fn format_nonexistent_file_returns_error() {
        let result = format(std::path::Path::new("/nonexistent/test.voom"));
        assert!(result.is_err());
    }

    #[test]
    fn show_valid_policy() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.voom");
        std::fs::write(&file, MINIMAL_POLICY).unwrap();

        let result = show(&file, OutputFormat::Table);
        assert!(result.is_ok());
    }

    #[test]
    fn show_nonexistent_file_returns_error() {
        let result = show(
            std::path::Path::new("/nonexistent/test.voom"),
            OutputFormat::Table,
        );
        assert!(result.is_err());
    }

    #[test]
    fn show_invalid_policy_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.voom");
        std::fs::write(&file, "garbage content here").unwrap();

        let result = show(&file, OutputFormat::Table);
        assert!(result.is_err());
    }

    #[test]
    fn describe_json_includes_stable_fields_and_composition() {
        let policy = voom_dsl::compile_policy_with_bundled(
            r#"policy "child" extends "anime-base" {
                phase audio { extend keep audio where lang == eng }
                phase subtitles { keep subtitles where lang == eng }
            }"#,
        )
        .unwrap();

        let output = DescribeOutput::from_policy(&policy);
        let value = serde_json::to_value(output).unwrap();

        assert_eq!(value["policy"], "child");
        assert_eq!(value["extends_chain"], serde_json::json!(["anime-base"]));
        assert_eq!(value["phase_order"], serde_json::json!(policy.phase_order));
        let phase_names: Vec<&str> = value["phases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|phase| phase["name"].as_str().unwrap())
            .collect();
        assert_eq!(phase_names, policy.phase_order);
        assert!(value["metadata"].get("version").is_some());
        assert!(value["metadata"].get("author").is_some());
        assert!(value["metadata"].get("description").is_some());
        assert!(value["metadata"].get("requires_voom").is_some());
        assert!(value["metadata"].get("requires_tools").is_some());
        assert!(value["metadata"].get("test_fixtures").is_some());
        assert_describe_phase(&value, "containerize", "Inherited", Some("anime-base"), 0);
        assert_describe_phase(&value, "audio", "Extended", Some("anime-base"), 1);
        assert_describe_phase(&value, "subtitles", "Overridden", Some("inline"), 0);

        let local_policy =
            voom_dsl::compile_policy(r#"policy "standalone" { phase local { keep audio } }"#)
                .unwrap();
        let local_output = serde_json::to_value(DescribeOutput::from_policy(&local_policy))
            .expect("describe output serializes");
        assert_describe_phase(&local_output, "local", "Local", None, 0);
    }

    #[test]
    fn describe_human_uses_sources_and_phase_order() {
        let mut policy = voom_dsl::compile_policy_with_bundled(
            r#"policy "child" extends "anime-base" {
                metadata { version: "2.0.0" }
                phase audio { extend keep audio where lang == eng }
                phase subtitles { keep subtitles where lang == eng }
            }"#,
        )
        .unwrap();
        policy.phases.reverse();

        let output = DescribeOutput::from_policy(&policy);
        let rendered = render_human_describe(&output, &policy.metadata.version);

        let row_order: Vec<&str> = rendered
            .lines()
            .filter_map(|line| line.strip_prefix("  "))
            .map(|line| line.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(row_order, policy.phase_order);
        assert!(rendered.contains("containerize  inherited from anime-base"));
        assert!(rendered.contains("audio         extended from anime-base (1 operation added)"));
        assert!(rendered.contains("subtitles     overridden by inline"));
    }

    fn assert_describe_phase(
        value: &Value,
        name: &str,
        kind: &str,
        source: Option<&str>,
        added_operations: usize,
    ) {
        let phase = value["phases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|phase| phase["name"] == name)
            .unwrap();
        assert_eq!(phase["name"], name);
        assert_eq!(phase["composition"]["kind"], kind);
        assert_eq!(phase["composition"]["source"], serde_json::json!(source));
        assert_eq!(phase["composition"]["added_operations"], added_operations);
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

        let result = diff(&a_file, &b_file, None, OutputFormat::Table);
        assert!(result.is_ok());
    }

    #[test]
    fn diff_fixture_identical_policies_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let policy_file = dir.path().join("policy.voom");
        let fixture_file = write_movie_fixture(dir.path());
        std::fs::write(&policy_file, MINIMAL_POLICY).unwrap();

        let result = diff(
            &policy_file,
            &policy_file,
            Some(&fixture_file),
            OutputFormat::Table,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn diff_fixture_different_plans_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let a_file = dir.path().join("a.voom");
        let b_file = dir.path().join("b.voom");
        let fixture_file = write_movie_fixture(dir.path());
        std::fs::write(&a_file, MINIMAL_POLICY).unwrap();
        std::fs::write(
            &b_file,
            r#"
policy "noop" {
  phase verify {
  }
}
"#,
        )
        .unwrap();

        let result = diff(&a_file, &b_file, Some(&fixture_file), OutputFormat::Table);

        assert!(result.is_err());
    }

    #[test]
    fn diff_nonexistent_file_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let a_file = dir.path().join("a.voom");
        std::fs::write(&a_file, MINIMAL_POLICY).unwrap();

        let result = diff(
            &a_file,
            std::path::Path::new("/nonexistent/b.voom"),
            None,
            OutputFormat::Table,
        );
        assert!(result.is_err());
    }

    fn write_movie_fixture(dir: &std::path::Path) -> std::path::PathBuf {
        let fixture_path = dir.join("movie.json");
        let fixture = Fixture {
            path: std::path::PathBuf::from("/media/movie.mp4"),
            container: Container::Mp4,
            duration: 120.0,
            size: 99,
            tracks: vec![Track::new(0, TrackType::Video, "h264".to_string())],
            capabilities: None,
        };
        std::fs::write(&fixture_path, serde_json::to_string(&fixture).unwrap()).unwrap();
        fixture_path
    }
}
