//! Immutable policy in pipeline revisions; durable verdict before completion delivery.
use crate::{
    engine::Engine,
    model::*,
    pipeline::{PipelineFile, StageKind},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Criterion {
    pub id: String,
    pub rule: Rule,
}
fn complete() -> bool {
    true
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Rule {
    StageSucceeded {
        stage_id: String,
    },
    Tests {
        stage_id: Option<String>,
        #[serde(default)]
        min_passed: u64,
        #[serde(default)]
        max_failed: u64,
        #[serde(default = "complete")]
        require_complete: bool,
    },
    Artifact {
        stage_id: String,
        name: String,
        #[serde(default)]
        min_bytes: u64,
    },
    Duration {
        stage_id: Option<String>,
        max_ms: u64,
    },
    LogText {
        stage_id: Option<String>,
        text: String,
        #[serde(default = "complete")]
        must_exist: bool,
    },
}
pub fn validate(file: &PipelineFile) -> Result<()> {
    ensure!(
        file.pipeline.acceptance.len() <= 32,
        invalid("at most 32 acceptance criteria")
    );
    let mut ids = std::collections::BTreeSet::new();
    for criterion in &file.pipeline.acceptance {
        crate::control::validate_name(&criterion.id)?;
        ensure!(
            ids.insert(&criterion.id),
            invalid("duplicate acceptance criterion")
        );
        let stage = match &criterion.rule {
            Rule::StageSucceeded { stage_id } | Rule::Artifact { stage_id, .. } => Some(stage_id),
            Rule::Tests { stage_id, .. }
            | Rule::Duration { stage_id, .. }
            | Rule::LogText { stage_id, .. } => stage_id.as_ref(),
        };
        if let Some(id) = stage {
            ensure!(
                file.pipeline.stages.iter().any(|s| &s.id == id),
                invalid("acceptance stage not found")
            );
        }
        match &criterion.rule {
            Rule::Tests { stage_id, .. } => ensure!(
                file.pipeline
                    .stages
                    .iter()
                    .any(|s| s.kind == StageKind::Test
                        && stage_id.as_ref().is_none_or(|id| id == &s.id)),
                invalid("test criterion requires test stages")
            ),
            Rule::Artifact { stage_id, name, .. } => ensure!(
                file.pipeline
                    .stages
                    .iter()
                    .any(|s| &s.id == stage_id && s.outputs.contains_key(name)),
                invalid("acceptance artifact is not declared")
            ),
            Rule::Duration { max_ms, .. } => ensure!(
                *max_ms > 0 && *max_ms <= 31_536_000_000,
                invalid("positive duration <= one year required")
            ),
            Rule::LogText { text, .. } => ensure!(
                !text.is_empty() && text.len() <= 2048,
                invalid("log criterion text must be 1..2048 bytes")
            ),
            _ => {}
        }
    }
    Ok(())
}
pub fn evaluate(e: &Engine, run: &Run) -> Result<Value> {
    let rules = run
        .pipeline
        .as_ref()
        .map(|p| p.pipeline.acceptance.as_slice())
        .unwrap_or(&[]);
    let mut results = Vec::new();
    for criterion in rules {
        let (passed, actual) = match &criterion.rule {
            Rule::StageSucceeded { stage_id } => {
                let stage = run
                    .stages
                    .iter()
                    .find(|s| &s.definition.id == stage_id)
                    .unwrap();
                (
                    Some(stage.status == RunState::Succeeded),
                    json!({"status":stage.status,"stage_id":stage_id}),
                )
            }
            Rule::Tests {
                stage_id,
                min_passed,
                max_failed,
                require_complete,
            } => {
                let mut counts = TestCounts::default();
                let mut known = true;
                let mut completeness = true;
                for stage in run.stages.iter().filter(|s| {
                    s.definition.kind == StageKind::Test
                        && stage_id.as_ref().is_none_or(|id| &s.definition.id == id)
                }) {
                    if let Some(report) = crate::testing::observed(&e.db, &run.id, stage)? {
                        counts.passed += report.counts.passed;
                        counts.failed += report.counts.failed;
                        counts.skipped += report.counts.skipped;
                        completeness &= report.cases_complete;
                    } else {
                        known = false;
                        completeness = false;
                    }
                }
                let pass = if !known || *require_complete && !completeness {
                    None
                } else {
                    Some(counts.passed >= *min_passed && counts.failed <= *max_failed)
                };
                (
                    pass,
                    json!({"counts":if known{Some(counts)}else{None},"cases_complete":completeness,"stage_id":stage_id}),
                )
            }
            Rule::Artifact {
                stage_id,
                name,
                min_bytes,
            } => {
                let artifact =
                    e.db.artifacts(&run.id)?
                        .into_iter()
                        .find(|a| &a.stage_id == stage_id && &a.name == name);
                (
                    Some(artifact.as_ref().is_some_and(|a| a.size >= *min_bytes)),
                    json!({"stage_id":stage_id,"artifact":name,"size":artifact.as_ref().map(|a|a.size),"sha256":artifact.map(|a|a.sha256)}),
                )
            }
            Rule::Duration { stage_id, max_ms } => {
                let elapsed = if let Some(id) = stage_id {
                    run.stages
                        .iter()
                        .find(|s| &s.definition.id == id)
                        .and_then(|s| Some(s.finished_at?.saturating_sub(s.started_at?)))
                } else {
                    Some(crate::db::now().saturating_sub(run.created_at))
                };
                (
                    elapsed.map(|ms| ms >= 0 && (ms as u64) <= *max_ms),
                    json!({"elapsed_ms":elapsed,"stage_id":stage_id}),
                )
            }
            Rule::LogText {
                stage_id,
                text,
                must_exist,
            } => {
                let c = e.db.connection()?;
                let found:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM log_events l JOIN log_search f ON f.rowid=l.id WHERE l.run_id=?1 AND (?2 IS NULL OR l.stage_id=?2) AND instr(f.text,?3)>0)",params![run.id,stage_id,text],|r|r.get(0))?;
                (
                    Some(found == *must_exist),
                    json!({"found":found,"stage_id":stage_id}),
                )
            }
        };
        results.push(json!({"id":criterion.id,"status":match passed{Some(true)=>"passed",Some(false)=>"failed",None=>"unknown"},"rule":criterion.rule,"actual":actual}));
    }
    let count = |status: &str| results.iter().filter(|r| r["status"] == status).count();
    let passed = count("passed");
    let failed = count("failed");
    let unknown = count("unknown");
    let status = if rules.is_empty() {
        "not_configured"
    } else if failed > 0 {
        "rejected"
    } else if unknown > 0 {
        "incomplete"
    } else {
        "accepted"
    };
    let report = json!({"run_id":run.id,"status":status,"passed":passed,"failed":failed,"unknown":unknown,"criteria":results});
    e.db.connection()?.execute(
        "INSERT INTO acceptance_results VALUES (?1,?2,?3)",
        params![run.id, report.to_string(), crate::db::now()],
    )?;
    Ok(report)
}
pub fn read(e: &Engine, id: &str) -> Result<Value> {
    let run = e.db.run(id)?;
    let c = e.db.connection()?;
    let report: Option<String> = c
        .query_row(
            "SELECT report_json FROM acceptance_results WHERE run_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match report {
        Some(text) => serde_json::from_str(&text)?,
        None => {
            let criteria = run
                .pipeline
                .as_ref()
                .map(|p| p.pipeline.acceptance.as_slice())
                .unwrap_or(&[]);
            let status = if criteria.is_empty() {
                "not_configured"
            } else if run.status.terminal() {
                "not_evaluated"
            } else {
                "pending"
            };
            json!({"run_id":id,"status":status,"criteria":criteria.iter().map(|c|json!({"id":c.id,"rule":c.rule,"status":"pending","actual":{}})).collect::<Vec<_>>()})
        }
    })
}
