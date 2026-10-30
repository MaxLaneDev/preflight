use crate::{
    db::Db,
    executor::{self, Execution, ProcessOutput},
    model::*,
    pipeline::{self, PipelineFile, StageDefinition, StageKind},
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 64 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FixtureAction {
    pub adapter: Adapter,
    pub suite: Suite,
    #[serde(default = "dot")]
    pub baseline_root: String,
}
fn dot() -> String {
    ".".into()
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Adapter {
    pub name: String,
    pub source_extension: String,
    pub compile: Template,
    pub run: Option<Template>,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    pub capabilities: Vec<Mode>,
    pub diagnostics_regex: Option<String>,
}
fn timeout() -> u64 {
    30
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub version: u32,
    pub suite: String,
    pub adapter: String,
    pub cases: Vec<Case>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub stdin: String,
    pub execute: CaseExecution,
    pub expect: Expect,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaseExecution {
    pub mode: Mode,
    pub entry: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Compile,
    CompileAndRun,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    pub compile: ProcessExpectation,
    pub run: Option<ProcessExpectation>,
    pub diagnostics: Option<Comparator>,
    #[serde(default)]
    pub files: BTreeMap<String, Comparator>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcessExpectation {
    pub exit_code: i32,
    pub stdout: Option<Comparator>,
    pub stderr: Option<Comparator>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Comparator {
    Exact(String),
    Trim(String),
    Contains(String),
    Regex(String),
    Json(serde_json::Value),
    Numeric(Numeric),
    Sha256(String),
    Snapshot(String),
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Numeric {
    pub value: f64,
    pub tolerance: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchFile {
    pub version: u32,
    pub build: PipelineFile,
    pub compiler: String,
    pub adapter: Adapter,
    pub suites: Vec<String>,
    #[serde(default = "parallel")]
    pub max_parallel_groups: usize,
}
fn parallel() -> usize {
    10
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartBatch {
    pub project_id: String,
    #[serde(default = "batch_path")]
    pub path: String,
}
fn batch_path() -> String {
    "preflight-fixtures.yaml".into()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub target: String,
    pub passed: bool,
    pub expected: String,
    pub actual: String,
    pub diff: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    pub case_id: String,
    pub status: RunState,
    pub checks: Vec<Check>,
    pub compile_exit: Option<i32>,
    pub run_exit: Option<i32>,
    pub reason: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
fn identifier(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'),
        invalid("invalid fixture identifier")
    );
    Ok(())
}
fn regex(pattern: &str) -> Result<regex::Regex> {
    regex::RegexBuilder::new(pattern)
        .size_limit(1024 * 1024)
        .build()
        .map_err(|e| invalid(format!("invalid fixture regex: {e}")))
}
fn validate_comparator(c: &Comparator) -> Result<()> {
    match c {
        Comparator::Regex(s) => {
            regex(s)?;
        }
        Comparator::Numeric(n) => ensure!(
            n.value.is_finite() && n.tolerance.is_finite() && n.tolerance >= 0.,
            invalid("numeric comparator needs finite value and nonnegative tolerance")
        ),
        Comparator::Sha256(s) => ensure!(
            s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()),
            invalid("expected SHA-256 must contain 64 hex digits")
        ),
        Comparator::Snapshot(s) => {
            ensure!(
                !pipeline::relative(s)?.as_os_str().is_empty(),
                invalid("empty snapshot path")
            );
        }
        _ => {}
    }
    Ok(())
}
pub fn validate(action: &FixtureAction) -> Result<()> {
    let a = &action.adapter;
    let suite = &action.suite;
    ensure!(
        serde_json::to_vec(action)?.len() <= LIMIT,
        invalid("fixture group exceeds 64 KiB")
    );
    ensure!(suite.version == 1, invalid("unsupported fixture version"));
    identifier(&suite.suite)?;
    identifier(&a.name)?;
    pipeline::relative(&action.baseline_root)?;
    ensure!(suite.adapter == a.name, invalid("suite adapter mismatch"));
    ensure!(
        !a.source_extension.is_empty()
            && a.source_extension.len() <= 16
            && a.source_extension
                .bytes()
                .all(|b| b.is_ascii_alphanumeric()),
        invalid("source_extension must be a plain extension")
    );
    ensure!(
        (1..=3600).contains(&a.timeout_seconds),
        invalid("invalid adapter timeout")
    );
    ensure!(
        !a.capabilities.is_empty() && a.capabilities.len() <= 2,
        invalid("invalid adapter capabilities")
    );
    for t in std::iter::once(&a.compile).chain(a.run.iter()) {
        pipeline::validate_command(&CommandSpec {
            program: t.program.clone(),
            args: t.args.clone(),
            timeout_seconds: a.timeout_seconds,
        })?;
        for v in std::iter::once(&t.program).chain(&t.args) {
            ensure!(
                !v.contains(['{', '}'])
                    || ["{compiler}", "{entry}", "{output}"].contains(&v.as_str()),
                invalid("adapter placeholders must occupy a whole argv value")
            );
        }
    }
    ensure!(
        a.compile.program == "{compiler}",
        invalid("compile program must be the supplied compiler")
    );
    if let Some(p) = &a.diagnostics_regex {
        regex(p)?;
    }
    ensure!(
        (1..=100).contains(&suite.cases.len()),
        invalid("suite must contain 1..100 cases")
    );
    let mut ids = BTreeSet::new();
    for c in &suite.cases {
        identifier(&c.id)?;
        ensure!(ids.insert(&c.id), invalid("duplicate fixture case"));
        ensure!(
            a.capabilities.contains(&c.execute.mode),
            invalid("adapter does not support fixture mode")
        );
        ensure!(
            c.files.contains_key(&c.execute.entry)
                && Path::new(&c.execute.entry)
                    .extension()
                    .is_some_and(|v| v == a.source_extension.as_str()),
            invalid("fixture entry must exist with adapter source extension")
        );
        ensure!(
            !c.files.is_empty() && c.files.len() <= 32 && c.expect.files.len() <= 16,
            invalid("too many fixture files")
        );
        let mut paths = BTreeSet::new();
        for name in c.files.keys() {
            let p = pipeline::relative(name)?;
            ensure!(
                !p.as_os_str().is_empty()
                    && p != Path::new("fixture-output.exe")
                    && paths.insert(p),
                invalid("colliding or reserved fixture file")
            );
        }
        for x in &paths {
            for y in &paths {
                ensure!(
                    x == y || !x.starts_with(y),
                    invalid("overlapping fixture files")
                );
            }
        }
        for path in c.expect.files.keys() {
            ensure!(
                !pipeline::relative(path)?.as_os_str().is_empty(),
                invalid("empty expected file path")
            );
        }
        let runs = c.execute.mode == Mode::CompileAndRun && c.expect.compile.exit_code == 0;
        ensure!(
            c.expect.run.is_some() == runs && (!runs || a.run.is_some()),
            invalid("run expectation/template must match compile_and_run success")
        );
        ensure!(
            c.expect.diagnostics.is_none() || a.diagnostics_regex.is_some(),
            invalid("diagnostics expectation requires adapter parser")
        );
        for comparator in c
            .expect
            .compile
            .stdout
            .iter()
            .chain(&c.expect.compile.stderr)
            .chain(
                c.expect
                    .run
                    .iter()
                    .flat_map(|e| e.stdout.iter().chain(&e.stderr)),
            )
            .chain(&c.expect.diagnostics)
            .chain(c.expect.files.values())
        {
            validate_comparator(comparator)?;
        }
    }
    Ok(())
}
pub fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_file(),
        "expected regular fixture file"
    );
    let f = std::fs::File::open(path)?;
    ensure!(f.metadata()?.is_file(), "expected regular file");
    let mut bytes = Vec::new();
    f.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit,
        "fixture input/output exceeds {limit} bytes"
    );
    Ok(bytes)
}
pub fn load(root: &Path, path: &str) -> Result<PipelineFile> {
    let preferred = crate::control::config_path(root, ".preflight/fixtures.yaml")?;
    let path = if path == "preflight-fixtures.yaml" && preferred.exists() {
        preferred
    } else if path.starts_with(".preflight/") {
        crate::control::config_path(root, path)?
    } else {
        pipeline::contained(root, path)?
    };
    let batch: BatchFile = serde_saphyr::from_slice(&read(&path, LIMIT)?)
        .map_err(|e| invalid(format!("invalid batch YAML: {e}")))?;
    ensure!(
        batch.version == 1
            && (1..=16).contains(&batch.suites.len())
            && (1..=32).contains(&batch.max_parallel_groups),
        invalid("invalid batch version/group count/concurrency")
    );
    ensure!(
        batch
            .build
            .pipeline
            .stages
            .iter()
            .all(|s| s.fixtures.is_none()),
        invalid("batch build cannot contain fixture groups")
    );
    pipeline::validate(&batch.build)?;
    let (producer, _) = pipeline::artifact_ref(&batch.compiler)?
        .ok_or_else(|| invalid("batch compiler must reference a build artifact"))?;
    let mut plan = batch.build;
    for path in &batch.suites {
        let suite_path = pipeline::contained(root, path)?;
        let suite: Suite = serde_saphyr::from_slice(&read(&suite_path, LIMIT)?)
            .map_err(|e| invalid(format!("invalid suite YAML: {e}")))?;
        let mut stage = StageDefinition::native(CommandSpec {
            program: batch.compiler.clone(),
            args: vec![],
            timeout_seconds: batch.adapter.timeout_seconds,
        });
        stage.id = format!("fixtures-{}", suite.suite);
        stage.needs = vec![producer.to_string()];
        stage.kind = StageKind::Test;
        stage.fixtures = Some(FixtureAction {
            adapter: batch.adapter.clone(),
            suite,
            baseline_root: Path::new(path)
                .parent()
                .unwrap_or(Path::new("."))
                .to_string_lossy()
                .into_owned(),
        });
        plan.pipeline.stages.push(stage);
    }
    plan.pipeline.max_parallel_stages = batch.max_parallel_groups;
    pipeline::validate(&plan)?;
    Ok(plan)
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
pub fn compare(
    target: &str,
    comparator: &Comparator,
    actual: &[u8],
    baselines: &Path,
) -> Result<Check> {
    let actual_text = text(actual);
    let (passed, expected) = match comparator {
        Comparator::Exact(s) => (actual == s.as_bytes(), s.clone()),
        Comparator::Trim(s) => (
            std::str::from_utf8(actual).is_ok_and(|v| v.trim() == s.trim()),
            format!("trim: {s}"),
        ),
        Comparator::Contains(s) => (
            std::str::from_utf8(actual).is_ok_and(|v| v.contains(s)),
            format!("contains: {s}"),
        ),
        Comparator::Regex(s) => (
            std::str::from_utf8(actual).is_ok_and(|v| regex(s).is_ok_and(|r| r.is_match(v))),
            format!("regex: {s}"),
        ),
        Comparator::Json(v) => (
            serde_json::from_slice::<serde_json::Value>(actual).is_ok_and(|a| a == *v),
            serde_json::to_string(v)?,
        ),
        Comparator::Numeric(n) => (
            actual_text
                .trim()
                .parse::<f64>()
                .is_ok_and(|a| a.is_finite() && (a - n.value).abs() <= n.tolerance),
            format!("{} +/- {}", n.value, n.tolerance),
        ),
        Comparator::Sha256(s) => (
            crate::hashing::hex(Sha256::digest(actual)).eq_ignore_ascii_case(s),
            s.clone(),
        ),
        Comparator::Snapshot(p) => {
            let expected = read(&pipeline::contained(baselines, p)?, LIMIT)?;
            (actual == expected, text(&expected))
        }
    };
    let actual = if matches!(comparator, Comparator::Sha256(_)) {
        crate::hashing::hex(Sha256::digest(actual))
    } else {
        actual_text
    };
    let diff = if passed {
        String::new()
    } else {
        let offset = expected
            .bytes()
            .zip(actual.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(expected.len().min(actual.len()));
        format!(
            "first difference at byte {offset}; expected {} bytes, actual {} bytes",
            expected.len(),
            actual.len()
        )
    };
    Ok(Check {
        target: target.into(),
        passed,
        expected,
        actual,
        diff,
    })
}
fn command(
    t: &Template,
    compiler: &str,
    entry: &str,
    output: &str,
    timeout_seconds: u64,
) -> CommandSpec {
    let substitute = |s: &str| {
        match s {
            "{compiler}" => compiler,
            "{entry}" => entry,
            "{output}" => output,
            _ => s,
        }
        .to_string()
    };
    CommandSpec {
        program: substitute(&t.program),
        args: t.args.iter().map(|a| substitute(a)).collect(),
        timeout_seconds,
    }
}
fn check_process(
    target: &str,
    expected: &ProcessExpectation,
    actual: &ProcessOutput,
    baselines: &Path,
    checks: &mut Vec<Check>,
) -> Result<()> {
    checks.push(Check {
        target: format!("{target}.exit_code"),
        passed: actual.exit == Some(expected.exit_code),
        expected: expected.exit_code.to_string(),
        actual: actual.exit.map_or("signal".into(), |n| n.to_string()),
        diff: if actual.exit == Some(expected.exit_code) {
            String::new()
        } else {
            "exit code differs".into()
        },
    });
    if actual.truncated {
        checks.push(Check {
            target: format!("{target}.output"),
            passed: false,
            expected: "complete output".into(),
            actual: "truncated at 64 KiB per stream".into(),
            diff: "cannot accept incomplete evidence".into(),
        });
    }
    for (stream, comparator, bytes) in [
        ("stdout", &expected.stdout, &actual.stdout),
        ("stderr", &expected.stderr, &actual.stderr),
    ] {
        if let Some(c) = comparator {
            checks.push(compare(&format!("{target}.{stream}"), c, bytes, baselines)?);
        }
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn run_case(
    db: &Db,
    run_id: &str,
    stage: &StageDefinition,
    workspace: &Path,
    compiler: &str,
    container: Option<&crate::docker::Context>,
    case: &Case,
    directory: &Path,
    result: &mut CaseResult,
    cancel: &CancellationToken,
) -> Result<()> {
    let action = stage.fixtures.as_ref().context("fixture action")?;
    let a = &action.adapter;
    for (path, contents) in &case.files {
        let path = pipeline::contained(directory, path)?;
        std::fs::create_dir_all(path.parent().context("file parent")?)?;
        std::fs::write(path, contents)?;
    }
    let entry = directory
        .join(&case.execute.entry)
        .to_string_lossy()
        .into_owned();
    let output = directory
        .join("fixture-output.exe")
        .to_string_lossy()
        .into_owned();
    let baselines = pipeline::contained(workspace, &action.baseline_root)?;
    let compile = command(&a.compile, compiler, &entry, &output, a.timeout_seconds);
    let compiled = executor::execute_captured(
        db,
        Execution {
            container,
            log_mode: crate::executor::LogMode::All,
            verified_executable: None,
            run_id,
            stage_id: &stage.id,
            workspace,
            cwd: directory,
            command: &compile,
            env: &stage.env,
            stdin: None,
            capture_limit: LIMIT,
        },
        cancel,
    )
    .await?;
    result.compile_exit = compiled.exit;
    if matches!(compiled.status, RunState::Cancelled | RunState::TimedOut) {
        result.status = compiled.status;
        return Ok(());
    }
    check_process(
        "compile",
        &case.expect.compile,
        &compiled,
        &baselines,
        &mut result.checks,
    )?;
    if let Some(expected) = &case.expect.diagnostics {
        let regex = regex(
            a.diagnostics_regex
                .as_deref()
                .context("diagnostics parser")?,
        )?;
        let stderr = text(&compiled.stderr);
        let actual = regex
            .find_iter(&stderr)
            .map(|m| m.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        result.checks.push(compare(
            "diagnostics",
            expected,
            actual.as_bytes(),
            &baselines,
        )?);
    }
    if compiled.exit == Some(0)
        && let Some(expected) = &case.expect.run
    {
        let run = command(
            a.run.as_ref().context("run template")?,
            compiler,
            &entry,
            &output,
            a.timeout_seconds,
        );
        let actual = executor::execute_captured(
            db,
            Execution {
                container,
                log_mode: crate::executor::LogMode::All,
                verified_executable: None,
                run_id,
                stage_id: &stage.id,
                workspace,
                cwd: directory,
                command: &run,
                env: &stage.env,
                stdin: Some(case.stdin.as_bytes()),
                capture_limit: LIMIT,
            },
            cancel,
        )
        .await?;
        result.run_exit = actual.exit;
        if matches!(actual.status, RunState::Cancelled | RunState::TimedOut) {
            result.status = actual.status;
            return Ok(());
        }
        check_process("run", expected, &actual, &baselines, &mut result.checks)?;
    }
    for (path, c) in &case.expect.files {
        let file = pipeline::contained(directory, path)?;
        if !file.exists() {
            result.checks.push(Check {
                target: format!("file:{path}"),
                passed: false,
                expected: "file exists".into(),
                actual: "missing".into(),
                diff: "expected output file was not produced".into(),
            });
        } else {
            result.checks.push(compare(
                &format!("file:{path}"),
                c,
                &read(&file, LIMIT)?,
                &baselines,
            )?);
        }
    }
    result.status = if result.checks.iter().all(|c| c.passed) {
        RunState::Succeeded
    } else {
        RunState::Failed
    };
    Ok(())
}
pub async fn execute(
    db: &Db,
    run_id: &str,
    stage: &StageDefinition,
    workspace: &Path,
    compiler: &str,
    container: Option<&crate::docker::Context>,
    cancel: &CancellationToken,
) -> Result<(RunState, TestReport)> {
    let action = stage.fixtures.as_ref().context("fixture action")?;
    let root = workspace.join(".preflight/cases");
    std::fs::create_dir_all(&root)?;
    let mut report = TestReport::default();
    let mut status = RunState::Succeeded;
    for case in &action.suite.cases {
        if cancel.is_cancelled() {
            status = RunState::Cancelled;
            break;
        }
        let mut result = CaseResult {
            case_id: case.id.clone(),
            status: RunState::Running,
            checks: vec![],
            compile_exit: None,
            run_exit: None,
            reason: None,
            started_at: now(),
            finished_at: None,
        };
        db.save_fixture(run_id, &stage.id, &result)?;
        let before_log: i64 = db.connection()?.query_row(
            "SELECT COALESCE(MAX(id),0) FROM log_events WHERE run_id=?1 AND stage_id=?2",
            rusqlite::params![run_id, stage.id],
            |r| r.get(0),
        )?;
        db.append_stage_log(
            run_id,
            &stage.id,
            "fixture",
            0,
            &format!("case {}\n", case.id),
        )?;
        let directory = tempfile::Builder::new().prefix("case-").tempdir_in(&root)?;
        if let Err(error) = run_case(
            db,
            run_id,
            stage,
            workspace,
            compiler,
            container,
            case,
            directory.path(),
            &mut result,
            cancel,
        )
        .await
        {
            result.status = if cancel.is_cancelled() {
                RunState::Cancelled
            } else {
                RunState::InfrastructureError
            };
            result.reason = Some(format!("{error:#}").chars().take(1024).collect());
        }
        result.finished_at = Some(now());
        db.save_fixture(run_id, &stage.id, &result)?;
        db.connection()?.execute(
            "UPDATE log_events SET test_id=?3 WHERE run_id=?1 AND stage_id=?2 AND id>?4",
            rusqlite::params![run_id, stage.id, case.id, before_log],
        )?;
        if let Err(error) = directory.close() {
            result.status = RunState::InfrastructureError;
            result.reason = Some(format!("fixture cleanup failed: {error}"));
            db.save_fixture(run_id, &stage.id, &result)?;
        }
        if result.status == RunState::Succeeded {
            report.counts.passed += 1;
        } else {
            report.counts.failed += 1;
            let message = result.reason.clone().unwrap_or_else(|| {
                result
                    .checks
                    .iter()
                    .filter(|c| !c.passed)
                    .map(|c| format!("{}: {}", c.target, c.diff))
                    .collect::<Vec<_>>()
                    .join("; ")
            });
            report.merge(TestReport {
                counts: TestCounts::default(),
                failures: vec![TestFailure {
                    name: case.id.clone(),
                    class_name: action.suite.suite.clone(),
                    message,
                }],
                truncated: false,
                ..TestReport::default()
            });
            if status == RunState::Succeeded
                || matches!(
                    result.status,
                    RunState::InfrastructureError | RunState::Cancelled | RunState::TimedOut
                )
            {
                status = result.status;
            }
        }
    }
    Ok((status, report))
}

/// MCP keeps one-case evidence bounded; REST retains the complete captured values.
pub fn compact_diff(mut value: serde_json::Value) -> serde_json::Value {
    let mut truncated = false;
    if let Some(checks) = value["checks"].as_array_mut() {
        checks.sort_by_key(|c| c["passed"].as_bool().unwrap_or(false));
        truncated |= checks.len() > 3;
        checks.truncate(3);
        for check in checks {
            for key in ["expected", "actual"] {
                if let Some(s) = check[key].as_str()
                    && s.chars().count() > 2048
                {
                    check[key] = s.chars().take(2048).collect::<String>().into();
                    truncated = true;
                }
            }
        }
    }
    value["truncated"] = truncated.into();
    value["details_uri"] = format!(
        "/api/v1/runs/{}/fixtures/{}/{}",
        value["batch_id"].as_str().unwrap_or_default(),
        value["stage_id"].as_str().unwrap_or_default(),
        value["case_id"].as_str().unwrap_or_default()
    )
    .into();
    value
}
