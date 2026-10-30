//! Immutable experiment definitions reuse the same source/profile/execution engine.
use crate::{
    control::{self, ConfigRef, SaveDefinition},
    engine::Engine,
    model::{Fault, invalid},
    pipeline::{PipelineFile, StartPipeline},
    profiles::{self, Revision},
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub version: u32,
    pub pipeline: Revision,
    pub baseline_run: Option<String>,
    pub override_definition: Option<PipelineFile>,
    pub profile: Option<Revision>,
    pub source: Option<SourceRequest>,
    #[serde(default)]
    pub selected_fixtures: BTreeMap<String, Vec<String>>,
}
pub type Save = SaveDefinition<Definition>;
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Start {
    pub project_id: String,
    pub name: String,
    pub revision: u64,
    pub idempotency_key: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub project_id: String,
    pub run_id: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cancel {
    pub project_id: String,
    pub run_id: String,
    pub actor: crate::model::Actor,
    pub reason: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Compare {
    pub project_id: String,
    pub run_id: String,
    pub baseline_id: Option<String>,
    #[serde(default)]
    pub offset: usize,
}
fn shape(d: &Definition) -> Result<()> {
    ensure!(
        d.version == 1,
        invalid("unsupported experiment definition version")
    );
    control::validate_name(&d.pipeline.name)?;
    ensure!(
        d.pipeline.revision > 0,
        invalid("explicit pipeline revision required")
    );
    if let Some(p) = &d.profile {
        control::validate_name(&p.name)?;
        ensure!(
            p.revision > 0,
            invalid("explicit profile revision required")
        );
    }
    if let Some(s) = &d.source {
        s.identity()?;
    }
    if let Some(f) = &d.override_definition {
        crate::pipeline::validate(f)?;
    }
    ensure!(
        d.selected_fixtures.len() <= 32,
        invalid("too many fixture stage selections")
    );
    for (stage, ids) in &d.selected_fixtures {
        crate::pipeline::identifier(stage)?;
        ensure!(
            !ids.is_empty()
                && ids.len() <= 100
                && ids.iter().collect::<BTreeSet<_>>().len() == ids.len(),
            invalid("fixture selection must contain unique case IDs")
        );
    }
    Ok(())
}
fn resolve(e: &Engine, project: &str, d: &Definition) -> Result<PipelineFile> {
    shape(d)?;
    if let Some(id) = &d.baseline_run {
        let baseline = e.db.run(id)?;
        ensure!(baseline.project_id == project, Fault::NotFound);
        ensure!(
            baseline.status.terminal(),
            invalid("baseline must be a terminal run")
        );
    }
    let saved = e.db.pipeline_config(&ConfigRef {
        project_id: project.into(),
        name: d.pipeline.name.clone(),
        revision: Some(d.pipeline.revision),
    })?;
    let mut file = match &d.override_definition {
        Some(f) => f.clone(),
        None => serde_json::from_value(saved["definition"].clone())?,
    };
    for (name, ids) in &d.selected_fixtures {
        let stage = file
            .pipeline
            .stages
            .iter_mut()
            .find(|s| &s.id == name)
            .ok_or_else(|| invalid("selected fixture stage not found"))?;
        let fixture = stage.fixtures.as_mut().ok_or_else(|| {
            invalid(
                "selected stage has no fixture cases; use command overrides for other test runners",
            )
        })?;
        ensure!(
            ids.iter()
                .all(|id| fixture.suite.cases.iter().any(|c| &c.id == id)),
            invalid("unknown fixture case ID")
        );
        fixture.suite.cases.retain(|c| ids.contains(&c.id));
    }
    crate::pipeline::validate(&file)?;
    Ok(file)
}
pub fn validate(e: &Engine, r: &Save) -> Result<Value> {
    let mut file = resolve(e, &r.project_id, &r.definition)?;
    if let Some(p) = &r.definition.profile {
        let saved = profiles::get(
            e,
            &ConfigRef {
                project_id: r.project_id.clone(),
                name: p.name.clone(),
                revision: Some(p.revision),
            },
        )?;
        profiles::apply(
            &mut file,
            &serde_json::from_value(saved["definition"].clone())?,
        )?;
    }
    Ok(json!({"valid":true,"effective_definition":file}))
}
pub fn save(e: &Engine, r: &Save) -> Result<Value> {
    validate(e, r)?;
    let root = e.db.project(&r.project_id)?.root;
    // Serialize baseline publication against the expiry recheck and quarantine move.
    let c = e.db.connection()?;
    if let Some(baseline) = &r.definition.baseline_run {
        let expired: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM artifact_expiry WHERE run_id=?1)",
            [baseline],
            |r| r.get(0),
        )?;
        ensure!(
            !expired,
            invalid("artifact_expired: select another baseline")
        );
    }
    let mut v = control::save_document(&root, "experiments", r, shape)?;
    drop(c);
    v["experiment_id"] = format!("{}/{}", r.project_id, r.name).into();
    Ok(v)
}
pub fn get(e: &Engine, r: &ConfigRef) -> Result<Value> {
    let mut v = control::get_document(&e.db.project(&r.project_id)?.root, "experiments", r, shape)?;
    v["experiment_id"] = format!("{}/{}", r.project_id, r.name).into();
    Ok(v)
}
pub fn list(e: &Engine, r: &control::ProjectId) -> Result<Value> {
    control::list_documents(
        &e.db.project(&r.project_id)?.root,
        "experiments",
        &r.project_id,
    )
}
pub fn start(e: &Arc<Engine>, r: Start) -> Result<Value> {
    control::validate_name(&r.name)?;
    ensure!(
        r.revision > 0,
        invalid("explicit experiment revision required")
    );
    let mut submission = profiles::Submission::new(
        Some(r.idempotency_key.clone()),
        json!({"operation":"experiment_start","request":r}),
    )?;
    if let Some(run) = e.db.submitted(&r.project_id, &submission)? {
        return Ok(
            json!({"run_id":run.id,"experiment_id":format!("{}/{}",r.project_id,r.name),"revision":r.revision}),
        );
    }
    let saved = get(
        e,
        &ConfigRef {
            project_id: r.project_id.clone(),
            name: r.name.clone(),
            revision: Some(r.revision),
        },
    )?;
    let d: Definition = serde_json::from_value(saved["definition"].clone())?;
    let mut file = resolve(e, &r.project_id, &d)?;
    submission.request["experiment_definition"] = serde_json::to_value(&d)?;
    submission.request["experiment_sha256"] = saved["sha256"].clone();
    if let Some(p) = &d.profile {
        let saved = profiles::get(
            e,
            &ConfigRef {
                project_id: r.project_id.clone(),
                name: p.name.clone(),
                revision: Some(p.revision),
            },
        )?;
        let profile: profiles::Profile = serde_json::from_value(saved["definition"].clone())?;
        profiles::apply(&mut file, &profile)?;
        submission.request["profile_snapshot"] = serde_json::to_value(&profile)?;
        submission.request["profile_sha256"] = saved["sha256"].clone();
    }
    let run = e.start_pipeline_source(
        StartPipeline {
            project_id: r.project_id.clone(),
            definition: Some(file),
        },
        Some(submission),
        d.source,
    )?;
    Ok(
        json!({"run_id":run.id,"experiment_id":format!("{}/{}",r.project_id,r.name),"revision":r.revision}),
    )
}
fn metadata(e: &Engine, r: &Run) -> Result<Value> {
    use rusqlite::OptionalExtension;
    e.db.connection()?.query_row("SELECT name,revision,baseline_run FROM experiment_runs WHERE run_id=?1 AND project_id=?2",rusqlite::params![r.run_id,r.project_id],|row|Ok(json!({"name":row.get::<_,String>(0)?,"revision":row.get::<_,u64>(1)?,"baseline_run":row.get::<_,Option<String>>(2)?}))).optional()?.ok_or_else(||Fault::NotFound.into())
}
pub fn status(e: &Engine, r: &Run) -> Result<Value> {
    let experiment = metadata(e, r)?;
    let report = crate::api::compact_report(e, &r.run_id)?;
    Ok(json!({"experiment":experiment,"report":report}))
}
pub fn cancel(e: &Engine, r: Cancel) -> Result<Value> {
    metadata(
        e,
        &Run {
            project_id: r.project_id,
            run_id: r.run_id.clone(),
        },
    )?;
    let run = e.cancel(crate::model::CancelRun {
        run_id: r.run_id,
        actor: r.actor,
        reason: r.reason,
    })?;
    Ok(json!({"run_id":run.id,"status":run.status}))
}
pub fn compare(e: &Engine, r: &Compare) -> Result<Value> {
    let meta = metadata(
        e,
        &Run {
            project_id: r.project_id.clone(),
            run_id: r.run_id.clone(),
        },
    )?;
    let baseline = r
        .baseline_id
        .as_deref()
        .or_else(|| meta["baseline_run"].as_str())
        .ok_or_else(|| invalid("explicit baseline required"))?;
    compare_runs(
        e,
        &Compare {
            project_id: r.project_id.clone(),
            run_id: r.run_id.clone(),
            baseline_id: Some(baseline.into()),
            offset: r.offset,
        },
    )
}
pub fn compare_runs(e: &Engine, r: &Compare) -> Result<Value> {
    ensure!(
        r.offset <= 100000,
        invalid("comparison offset must be 0..100000")
    );
    let baseline = r
        .baseline_id
        .as_deref()
        .ok_or_else(|| invalid("explicit baseline required"))?;
    let current = e.db.run(&r.run_id)?;
    ensure!(current.project_id == r.project_id, Fault::NotFound);
    let old = e.db.run(baseline)?;
    ensure!(old.project_id == r.project_id, Fault::NotFound);
    let cases = |run: &crate::model::Run| {
        run.stages
            .iter()
            .flat_map(|s| {
                s.tests.iter().flat_map(|t| {
                    t.cases
                        .iter()
                        .map(|c| (format!("{}/{}", s.definition.id, c.id), c.status.clone()))
                })
            })
            .collect::<BTreeMap<_, _>>()
    };
    let a = cases(&current);
    let b = cases(&old);
    let names = current
        .stages
        .iter()
        .chain(old.stages.iter())
        .flat_map(|s| {
            s.tests.iter().flat_map(|t| {
                t.cases
                    .iter()
                    .map(|c| (format!("{}/{}", s.definition.id, c.id), c.name.clone()))
            })
        })
        .collect::<BTreeMap<_, _>>();
    let keys = a.keys().chain(b.keys()).collect::<BTreeSet<_>>();
    let changes = keys
        .into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .map(|k| json!({"test":k,"name":names.get(k),"baseline":b.get(k),"current":a.get(k)}))
        .collect::<Vec<_>>();
    let artifacts = |id: &str| -> Result<BTreeMap<String, String>> {
        Ok(e.db
            .artifacts(id)?
            .into_iter()
            .map(|a| (format!("{}/{}", a.stage_id, a.name), a.sha256))
            .collect())
    };
    let aa = artifacts(&r.run_id)?;
    let bb = artifacts(baseline)?;
    let artifact_changes = aa
        .keys()
        .chain(bb.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|k| aa.get(*k) != bb.get(*k))
        .map(|k| json!({"artifact":k,"baseline":bb.get(k),"current":aa.get(k)}))
        .collect::<Vec<_>>();
    let complete = |run: &crate::model::Run| {
        run.stages
            .iter()
            .filter(|s| s.definition.kind == crate::pipeline::StageKind::Test)
            .all(|s| s.tests.as_ref().is_some_and(|t| t.cases_complete))
    };
    let pins = e.db.toolchain(&r.run_id)?;
    let old_pins = e.db.toolchain(baseline)?;
    let input = match (e.db.inputs(&r.run_id)?, e.db.inputs(baseline)?) {
        (Some(a), Some(b)) => {
            json!({"run_id":r.run_id,"baseline_id":baseline,"diff":crate::inputs::compare(&a,&b,r.offset)?})
        }
        _ => json!({"available":false,"reason":"one or both runs have no captured input manifest"}),
    };
    let stages = |run: &crate::model::Run| {
        run.stages
            .iter()
            .map(|s| (s.definition.id.clone(), s.status.as_str()))
            .collect::<BTreeMap<_, _>>()
    };
    let sa = stages(&current);
    let sb = stages(&old);
    let stage_changes = sa
        .keys()
        .chain(sb.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|k| sa.get(*k) != sb.get(*k))
        .map(|k| json!({"stage_id":k,"baseline":sb.get(k),"current":sa.get(k)}))
        .collect::<Vec<_>>();
    let next = r.offset.saturating_add(100);
    let total = changes
        .len()
        .max(artifact_changes.len())
        .max(stage_changes.len());
    Ok(
        json!({"project_id":r.project_id,"run_id":r.run_id,"baseline_id":baseline,"status":{"current":current.status,"baseline":old.status},"test_changes_total":changes.len(),"artifact_changes_total":artifact_changes.len(),"stage_changes_total":stage_changes.len(),"stage_changes":stage_changes.into_iter().skip(r.offset).take(100).collect::<Vec<_>>(),"test_changes":changes.into_iter().skip(r.offset).take(100).collect::<Vec<_>>(),"artifact_changes":artifact_changes.into_iter().skip(r.offset).take(100).collect::<Vec<_>>(),"next_cursor":(next<total).then_some(next),"inputs":input,"test_evidence_complete":complete(&current)&&complete(&old),"toolchain_equal":match (pins,old_pins){(Some(a),Some(b))=>Some(a.contract_sha256==b.contract_sha256),_=>None},"performance_comparison":"not asserted; requires equivalent inputs and measured workload"}),
    )
}
