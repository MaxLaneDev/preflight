//! Text diagnostics and immutable project pipeline revision restoration.
use crate::{
    control,
    engine::Engine,
    model::{Fault, invalid},
    pipeline::{self, PipelineFile},
};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::Deserialize;
use serde_json::{Value, json};
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub text: String,
}
pub fn check(r: &Check) -> Result<Value> {
    ensure!(r.text.len() <= 65536, invalid("definition exceeds 64 KiB"));
    let mut reader = serde_json::Deserializer::from_str(&r.text);
    let file: PipelineFile = match serde_path_to_error::deserialize(&mut reader) {
        Ok(v) => v,
        Err(error) => {
            let inner = error.inner();
            return Ok(
                json!({"valid":false,"diagnostics":[{"severity":"error","message":inner.to_string(),"line":inner.line().max(1),"column":inner.column().max(1),"path":error.path().to_string()}]}),
            );
        }
    };
    if let Err(error) = reader.end() {
        return Ok(
            json!({"valid":false,"diagnostics":[{"severity":"error","message":error.to_string(),"line":error.line(),"column":error.column()}]}),
        );
    }
    match pipeline::validate(&file) {
        Ok(()) => Ok(json!({"valid":true,"diagnostics":[]})),
        Err(error) => {
            let message = error.to_string();
            let path = if file.version != 1 {
                "version"
            } else if file.pipeline.stages.is_empty() {
                "pipeline.stages"
            } else {
                "pipeline"
            };
            Ok(
                json!({"valid":false,"diagnostics":[{"severity":"error","message":message,"line":1,"column":1,"path":path}]}),
            )
        }
    }
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Revisions {
    pub project_id: String,
    pub name: String,
    pub before: Option<u64>,
    #[serde(default = "limit")]
    pub limit: usize,
}
fn limit() -> usize {
    25
}
pub fn revisions(e: &Engine, r: &Revisions) -> Result<Value> {
    ensure!(
        (1..=100).contains(&r.limit),
        invalid("limit must be 1..100")
    );
    control::validate_name(&r.name)?;
    let root = e.db.project(&r.project_id)?.root;
    let current = control::latest(&root, "pipelines", &r.name)?;
    ensure!(current > 0, Fault::NotFound);
    let top = r
        .before
        .unwrap_or(current.saturating_add(1))
        .min(current.saturating_add(1));
    let mut items = Vec::new();
    for revision in (1..top).rev().take(r.limit) {
        let saved = control::get(
            &root,
            &control::ConfigRef {
                project_id: r.project_id.clone(),
                name: r.name.clone(),
                revision: Some(revision),
            },
        )?;
        items.push(json!({"revision":revision,"sha256":saved["sha256"],"path":format!(".preflight/pipelines/{}/{revision}.json",r.name)}));
    }
    let next = items
        .last()
        .and_then(|v| v["revision"].as_u64())
        .filter(|v| *v > 1);
    Ok(
        json!({"project_id":r.project_id,"name":r.name,"current_revision":current,"items":items,"next_cursor":next}),
    )
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Restore {
    pub project_id: String,
    pub name: String,
    pub source_revision: u64,
    pub expected_revision: u64,
}
pub fn restore(e: &Engine, r: &Restore) -> Result<Value> {
    let root = e.db.project(&r.project_id)?.root;
    let old = control::get(
        &root,
        &control::ConfigRef {
            project_id: r.project_id.clone(),
            name: r.name.clone(),
            revision: Some(r.source_revision),
        },
    )?;
    let mut saved = control::save(
        &root,
        &control::SaveDefinition {
            project_id: r.project_id.clone(),
            name: r.name.clone(),
            expected_revision: r.expected_revision,
            definition: serde_json::from_value(old["definition"].clone())?,
        },
    )?;
    saved["restored_from"] = r.source_revision.into();
    Ok(saved)
}
