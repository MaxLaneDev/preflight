//! Language-independent report adapters and a common observed test feed.
use crate::{
    db::Db,
    model::*,
    pipeline::{self, StageDefinition, StageKind},
};
use anyhow::{Context, Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerun: Option<crate::model::CommandSpec>,
    pub kind: String,
    #[serde(default)]
    pub reports: Vec<Report>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub format: Format,
    pub path: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Auto,
    Junit,
    PreflightJson,
}
pub fn kind(stage: &StageDefinition) -> &str {
    stage
        .testing
        .as_ref()
        .map(|t| t.kind.as_str())
        .unwrap_or(if stage.fixtures.is_some() {
            "fixture"
        } else if stage.kind == StageKind::Test {
            "custom"
        } else {
            "build"
        })
}
pub fn validate(stage: &StageDefinition) -> Result<()> {
    if let Some(testing) = &stage.testing {
        crate::control::validate_name(&testing.kind)?;
        if let Some(command) = &testing.rerun {
            crate::pipeline::validate_command(command)?;
        }
        ensure!(
            stage.kind == StageKind::Test,
            invalid("testing configuration requires a test stage")
        );
        ensure!(
            testing.reports.len() + stage.junit.len() <= 16,
            invalid("at most 16 test reports")
        );
        ensure!(
            stage.fixtures.is_none() || testing.reports.is_empty(),
            invalid("native fixtures produce their own reports")
        );
        let mut seen = std::collections::BTreeSet::new();
        for path in stage.report_paths() {
            ensure!(
                !path.is_empty() && path != "." && seen.insert(pipeline::relative(path)?),
                invalid("duplicate or empty test report path")
            );
        }
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonReport {
    version: u32,
    cases: Vec<JsonCase>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonCase {
    name: String,
    status: String,
    #[serde(default)]
    suite: String,
    #[serde(default)]
    class_name: String,
    #[serde(default)]
    message: String,
    file: Option<String>,
    line: Option<u64>,
    duration_seconds: Option<f64>,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
}
pub fn parse_json(text: &str) -> Result<TestReport> {
    ensure!(text.len() <= 4 * 1024 * 1024, "test report exceeds 4 MiB");
    let input: JsonReport = serde_json::from_str(text)?;
    ensure!(
        input.version == 1 && input.cases.len() <= 10000,
        "report version 1 and at most 10000 cases required"
    );
    let mut report = TestReport {
        cases_complete: true,
        ..Default::default()
    };
    let mut occurrences = std::collections::BTreeMap::<String, u64>::new();
    for c in input.cases {
        ensure!(
            !c.name.trim().is_empty()
                && c.name.len() <= 256
                && c.suite.len() <= 256
                && c.class_name.len() <= 256
                && c.message.len() <= 2048,
            "bounded test name/suite/message required"
        );
        ensure!(
            c.stdout.len() <= 65536 && c.stderr.len() <= 65536,
            "test output exceeds 64 KiB per stream"
        );
        if let Some(path) = &c.file {
            pipeline::relative(path)?;
        }
        ensure!(
            c.line.is_none_or(|v| v > 0 && v <= 10_000_000)
                && c.duration_seconds.is_none_or(|v| v.is_finite() && v >= 0.0),
            "invalid source line or duration"
        );
        match c.status.as_str() {
            "passed" => report.counts.passed += 1,
            "failed" => report.counts.failed += 1,
            "skipped" => report.counts.skipped += 1,
            _ => anyhow::bail!("case status must be passed, failed or skipped"),
        }
        let key = serde_json::to_string(&(&c.suite, &c.class_name, &c.name))?;
        let occurrence = occurrences.entry(key.clone()).or_default();
        let id = crate::hashing::hex(Sha256::digest(format!("{key}:{occurrence}").as_bytes()));
        *occurrence += 1;
        if c.status == "failed" && report.failures.len() < 20 {
            report.failures.push(TestFailure {
                name: c.name.clone(),
                class_name: c.class_name.clone(),
                message: c.message.clone(),
            });
        }
        let output = (!c.stdout.is_empty() || !c.stderr.is_empty()).then_some(TestOutput {
            stdout: c.stdout,
            stderr: c.stderr,
            truncated: false,
        });
        report.cases.push(TestCase {
            id,
            name: c.name,
            class_name: c.class_name,
            suite: c.suite,
            status: c.status,
            message: c.message,
            file: c.file,
            line: c.line,
            duration_seconds: c.duration_seconds,
            output,
        });
    }
    report.truncated = report.counts.failed > 20;
    Ok(report)
}
pub fn read(stage: &StageDefinition, cwd: &Path) -> Result<Option<TestReport>> {
    let mut combined = TestReport::default();
    let mut present = false;
    for path in &stage.junit {
        let mut report = crate::junit::read(&pipeline::contained(cwd, path)?)?;
        if stage.report_paths().count() > 1 {
            namespace(&mut report, path);
        }
        combined.merge(report);
        present = true;
    }
    if let Some(configuration) = &stage.testing {
        for input in &configuration.reports {
            let path = pipeline::contained(cwd, &input.path)?;
            let file = std::fs::File::open(&path)
                .with_context(|| format!("missing test report {}", input.path))?;
            ensure!(
                file.metadata()?.is_file(),
                "test report is not a regular file"
            );
            let mut bytes = Vec::new();
            file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            ensure!(bytes.len() <= 4 * 1024 * 1024, "test report exceeds 4 MiB");
            let text = std::str::from_utf8(&bytes)?;
            let junit = matches!(input.format, Format::Junit)
                || matches!(input.format, Format::Auto)
                    && text
                        .trim_start_matches('\u{feff}')
                        .trim_start()
                        .starts_with('<');
            let mut report = if junit {
                crate::junit::parse(text)?
            } else {
                parse_json(text)?
            };
            if stage.report_paths().count() > 1 {
                namespace(&mut report, &input.path);
            }
            combined.merge(report);
            present = true;
        }
    }
    Ok(present.then_some(combined))
}
pub fn observed(db: &Db, run: &str, stage: &StageRun) -> Result<Option<TestReport>> {
    let Some(action) = &stage.definition.fixtures else {
        return Ok(stage.tests.clone());
    };
    let c = db.connection()?;
    let mut statement=c.prepare("SELECT case_id,status,result_json FROM fixture_results WHERE run_id=?1 AND stage_id=?2 ORDER BY ordinal")?;
    let rows = statement
        .query_map(rusqlite::params![run, stage.definition.id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut report = TestReport {
        cases_complete: stage.status.terminal() && rows.len() == action.suite.cases.len(),
        ..Default::default()
    };
    for (id, status, result) in rows {
        let result = result
            .map(|s| serde_json::from_str::<serde_json::Value>(&s))
            .transpose()?;
        let normalized = match status.as_str() {
            "succeeded" => {
                report.counts.passed += 1;
                "passed"
            }
            "failed" | "timed_out" | "infrastructure_error" => {
                report.counts.failed += 1;
                "failed"
            }
            "skipped" | "cancelled" | "blocked" => {
                report.counts.skipped += 1;
                "skipped"
            }
            other => {
                report.cases_complete = false;
                other
            }
        };
        let message = result
            .as_ref()
            .and_then(|r| r["reason"].as_str())
            .unwrap_or("")
            .chars()
            .take(2048)
            .collect::<String>();
        report.cases.push(TestCase {
            id: id.clone(),
            name: id,
            class_name: action.suite.suite.clone(),
            suite: action.suite.suite.clone(),
            status: normalized.into(),
            message,
            file: None,
            line: None,
            duration_seconds: result.as_ref().and_then(|r| {
                Some((r["finished_at"].as_i64()? - r["started_at"].as_i64()?) as f64 / 1000.0)
            }),
            output: None,
        });
    }
    Ok(Some(report))
}

fn namespace(report: &mut TestReport, path: &str) {
    for case in &mut report.cases {
        case.id = crate::hashing::hex(Sha256::digest(format!("{path}:{}", case.id).as_bytes()));
    }
}
