//! Explicit partial reruns; original executions and their verdicts remain immutable.
use crate::{
    engine::Engine,
    model::{Actor, Artifact, Fault, Run, RunState, invalid},
    pipeline::{self, PipelineFile, StageDefinition, StartPipeline},
    profiles::Submission,
    sources::SourceRequest,
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub project_id: String,
    pub run_id: String,
    pub stage_id: String,
    pub test_id: Option<String>,
    pub stage_override: Option<StageDefinition>,
    /// Omit to capture current project files; explicit Git/dirty/multi requests are supported.
    pub source: Option<SourceRequest>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub selection: Selection,
    pub idempotency_key: String,
    pub actor: Actor,
    pub reason: String,
}
struct Plan {
    file: PipelineFile,
    artifacts: Vec<Artifact>,
    retry: Value,
    identity: Value,
    profile: Option<Value>,
}
fn plan(e: &Engine, r: &Selection) -> Result<Plan> {
    let parent = e.db.run(&r.run_id)?;
    ensure!(
        parent.external.is_none(),
        invalid("external CI imports are read-only; native stage retry is unavailable")
    );
    ensure!(parent.project_id == r.project_id, Fault::NotFound);
    ensure!(
        parent.status.terminal(),
        invalid("retry requires a terminal parent run")
    );
    let original = parent
        .stages
        .iter()
        .find(|s| s.definition.id == r.stage_id)
        .ok_or(Fault::NotFound)?;
    let mut stage = r
        .stage_override
        .clone()
        .unwrap_or_else(|| original.definition.clone());
    ensure!(
        stage.id == r.stage_id,
        invalid("stage override must retain selected stage ID")
    );
    ensure!(
        serde_json::to_vec(&stage)?.len() <= 65536,
        invalid("stage override exceeds 64 KiB")
    );
    if let Some(test) = &r.test_id {
        if let Some(fixtures) = stage.fixtures.as_mut() {
            ensure!(
                original.definition.fixtures.as_ref().is_some_and(|f| f
                    .suite
                    .cases
                    .iter()
                    .any(|c| &c.id == test)),
                invalid("test was not in the original fixture stage")
            );
            ensure!(
                fixtures.suite.cases.iter().any(|c| &c.id == test),
                invalid("selected fixture is absent from override")
            );
            fixtures.suite.cases.retain(|c| &c.id == test);
        } else {
            let case = original
                .tests
                .as_ref()
                .and_then(|t| t.cases.iter().find(|c| &c.id == test))
                .ok_or_else(|| invalid("unknown recorded test ID"))?;
            let mut command = stage
                .testing
                .as_ref()
                .and_then(|t| t.rerun.clone())
                .ok_or_else(|| {
                    invalid(
                        "single-test rerun requires testing.rerun command; retry the stage instead",
                    )
                })?;
            ensure!(
                command
                    .args
                    .iter()
                    .any(|a| a == "${test.name}" || a == "${test.id}"),
                invalid(
                    "testing.rerun must select a test by a whole ${test.name} or ${test.id} argument"
                )
            );
            for arg in &mut command.args {
                if arg.starts_with("${test.") {
                    let value = match arg.as_str() {
                        "${test.name}" => Some(case.name.as_str()),
                        "${test.id}" => Some(case.id.as_str()),
                        "${test.file}" => case.file.as_deref(),
                        "${test.class_name}" => Some(case.class_name.as_str()),
                        "${test.suite}" => Some(case.suite.as_str()),
                        _ => None,
                    };
                    ensure!(
                        value.is_none_or(|v| !v.contains("${")),
                        invalid(
                            "test selector contains execution template syntax; use a stage retry"
                        )
                    );
                }

                *arg = match arg.as_str() {
                    "${test.name}" => case.name.clone(),
                    "${test.id}" => case.id.clone(),
                    "${test.class_name}" => case.class_name.clone(),
                    "${test.suite}" => case.suite.clone(),
                    "${test.file}" => case
                        .file
                        .clone()
                        .ok_or_else(|| invalid("selected test has no source file"))?,
                    other => other.to_owned(),
                };
            }
            ensure!(
                !command.args.iter().any(|a| a.contains("${test.")),
                invalid("test placeholders must occupy a whole argument")
            );
            stage.command = command;
        }
    }
    // Dependencies are explicitly reused, never implicitly re-executed.
    for dep in &stage.needs {
        ensure!(
            dep != &stage.id
                && parent
                    .stages
                    .iter()
                    .any(|s| &s.definition.id == dep && s.status == RunState::Succeeded),
            invalid(format!("dependency {dep} did not succeed; retry it first"))
        );
    }
    let available = e.db.artifacts(&parent.id)?;
    let mut reused = BTreeMap::new();
    let mut commands = vec![&mut stage.command];
    if let Some(command) = stage.testing.as_mut().and_then(|t| t.rerun.as_mut()) {
        commands.push(command);
    }
    for value in commands
        .into_iter()
        .flat_map(|c| std::iter::once(&mut c.program).chain(&mut c.args))
        .chain(stage.inputs.values_mut())
    {
        if let Some((dep, name)) = pipeline::artifact_ref(value)? {
            ensure!(
                stage.needs.iter().any(|v| v == dep),
                invalid("artifact reference must name a declared dependency")
            );
            let artifact = available
                .iter()
                .find(|a| a.stage_id == dep && a.name == name)
                .ok_or_else(|| invalid("dependency artifact is not published"))?;
            e.db.require_artifact_available(&parent.id, &artifact.stage_id)?;
            reused.insert(
                (artifact.stage_id.clone(), artifact.name.clone()),
                artifact.clone(),
            );
            *value = format!("${{module.{}}}", artifact.sha256);
        }
    }
    stage.needs.clear();
    stage.cache = None;
    let profile =
        e.db.submission(&parent.id)?
            .and_then(|s| s.get("profile_snapshot").cloned());
    let mut file = PipelineFile {
        version: 1,
        project: None,
        pipeline: pipeline::PipelineDefinition {
            acceptance: vec![],
            max_parallel_stages: 1,
            stages: vec![stage],
        },
    };
    if let Some(profile) = &profile {
        crate::profiles::apply(&mut file, &serde_json::from_value(profile.clone())?)?;
    } else {
        ensure!(
            !file.pipeline.stages[0].command.program.contains("${tool"),
            invalid("tool reference requires the original profile")
        );
    }
    pipeline::validate(&file)?;
    if let Some(source) = &r.source {
        match source {
            SourceRequest::Git(s) => s.validate()?,
            SourceRequest::Dirty(s) => s.validate()?,
            SourceRequest::Multi(s) => s.validate()?,
        }
    }
    let identity=e.db.connection()?.query_row("SELECT pipeline_key,name,saved_name FROM run_pipeline_identity WHERE run_id=?1",[&parent.id],|row|Ok(json!({"key":row.get::<_,String>(0)?,"name":row.get::<_,String>(1)?,"saved":row.get::<_,Option<String>>(2)?})))?;
    let artifacts = reused.into_values().collect::<Vec<_>>();
    let retry = json!({"parent_run_id":parent.id,"stage_id":r.stage_id,"test_id":r.test_id,"scope":"partial","source_mode":if r.source.is_some(){"explicit"}else{"current_project"},"reused_artifacts":artifacts.iter().map(|a|json!({"run_id":a.run_id,"stage_id":a.stage_id,"name":a.name,"sha256":a.sha256,"size":a.size})).collect::<Vec<_>>(),"whole_pipeline_verdict":false});

    Ok(Plan {
        file,
        artifacts,
        retry,
        identity,
        profile,
    })
}
pub fn preview(e: &Engine, r: &Selection) -> Result<Value> {
    let plan = plan(e, r)?;
    Ok(
        json!({"retry":plan.retry,"stage":{"id":plan.file.pipeline.stages[0].id,"kind":plan.file.pipeline.stages[0].kind,"command":plan.file.pipeline.stages[0].command,"fixture_cases":plan.file.pipeline.stages[0].fixtures.as_ref().map(|f|f.suite.cases.iter().map(|c|&c.id).collect::<Vec<_>>())},"reuse_policy":"explicit previous successful outputs; other stages and whole-pipeline criteria are not re-evaluated"}),
    )
}
pub fn start(e: &Arc<Engine>, r: Start) -> Result<Run> {
    ensure!(
        !r.reason.trim().is_empty() && r.reason.len() <= 1024,
        invalid("retry reason must be 1..1024 bytes")
    );
    let mut submission = Submission::new(
        Some(r.idempotency_key.clone()),
        json!({"operation":"pipeline_retry","request":r}),
    )?;
    if let Some(run) = e.db.submitted(&r.selection.project_id, &submission)? {
        return Ok(run);
    }
    ensure!(
        e.db.run(&r.selection.run_id)?.project_id == r.selection.project_id,
        Fault::NotFound
    );
    // Protect retained producer bytes through verified publication and submission.
    let _lease = e.db.begin_operation(
        &r.selection.run_id,
        "pipeline_retry",
        BTreeSet::from([format!("run:{}", r.selection.run_id)]),
    )?;
    let plan = plan(e, &r.selection)?;
    for artifact in &plan.artifacts {
        crate::modules::publish_stage(
            e,
            &crate::modules::Publish {
                run_id: artifact.run_id.clone(),
                stage_id: artifact.stage_id.clone(),
                artifact_name: artifact.name.clone(),
            },
        )?;
    }
    submission.request["retry"] = plan.retry;
    submission.request["pipeline_identity"] = plan.identity;
    if let Some(profile) = plan.profile {
        submission.request["profile_snapshot"] = profile;
    }
    e.start_pipeline_source(
        StartPipeline {
            project_id: r.selection.project_id,
            definition: Some(plan.file),
        },
        Some(submission),
        r.selection.source,
    )
}

pub(crate) fn report_error(
    e: &Engine,
    id: &str,
    report: Option<&crate::model::TestReport>,
) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let test: Option<Option<String>> = e
        .db
        .connection()?
        .query_row(
            "SELECT json_extract(request_json,'$.retry.test_id') FROM submissions WHERE run_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(test) = test.flatten() else {
        return Ok(None);
    };
    let valid = report.is_some_and(|r| {
        r.cases_complete
            && r.cases
                .iter()
                .any(|c| c.id == test && matches!(c.status.as_str(), "passed" | "failed"))
            && r.cases
                .iter()
                .all(|c| c.id == test || matches!(c.status.as_str(), "skipped" | "not_run"))
    });
    Ok((!valid).then(||"single-test runner did not report exactly the selected test; no successful single-test verdict".into()))
}
