//! Diagnostic stage identities. No cache lookup or reuse is authorized by these keys.
use crate::{db::Db, pipeline::StageDefinition};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const FORMAT: &str = "preflight-stage-evidence-v1";
#[derive(Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub format: String,
    pub sha256: String,
    pub components: BTreeMap<String, String>,
    pub reusable: bool,
    pub reason: String,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub run_id: String,
    pub stage_id: String,
    pub baseline_id: Option<String>,
}
fn digest(value: &impl Serialize) -> Result<String> {
    Ok(crate::hashing::hex(Sha256::digest(serde_json::to_vec(
        value,
    )?)))
}
fn assemble(components: BTreeMap<String, String>) -> Result<Evidence> {
    Ok(Evidence {
        format: FORMAT.into(),
        sha256: digest(&(FORMAT, &components))?,
        components,
        reusable: false,
        reason: "diagnostic identity only; inspect runs_cache for separately authorized build cache decisions".into(),
    })
}
pub fn capture(db: &Db, id: &str, stage: &StageDefinition) -> Result<Evidence> {
    let inputs = db
        .inputs(id)?
        .ok_or_else(|| anyhow::anyhow!("missing input manifest"))?;
    let toolchain = db.toolchain(id)?;
    let artifacts = db.artifacts(id)?.into_iter().filter(|a| stage.needs.contains(&a.stage_id))
        .map(|a| json!({"stage":a.stage_id,"name":a.name,"kind":a.kind,"sha256":a.sha256,"size":a.size})).collect::<Vec<_>>();
    // Hash bytes rather than lossy UTF-8; values (possibly secrets) are never persisted.
    let inherited = crate::executor::INHERITED_ENV
        .iter()
        .map(|name| {
            let value = std::env::var_os(name);
            (*name, value.map(|v| v.as_encoded_bytes().to_vec()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut components = BTreeMap::from([
        ("source_tree".into(), inputs.tree_sha256),
        ("configuration".into(), inputs.configuration_sha256),
        ("stage".into(), digest(stage)?),
        (
            "toolchain".into(),
            digest(&toolchain.map(|v| v.contract_sha256))?,
        ),
        (
            "host".into(),
            digest(
                &stage
                    .worker
                    .as_ref()
                    .map(|w| (w.os.as_str(), w.arch.as_str()))
                    .unwrap_or((std::env::consts::OS, std::env::consts::ARCH)),
            )?,
        ),
        ("inherited_environment".into(), digest(&inherited)?),
        ("dependency_artifacts".into(), digest(&artifacts)?),
    ]);
    if let Some(container) = &stage.container {
        components.insert(
            "container_runtime".into(),
            digest(&(
                crate::docker::RUNTIME_FORMAT,
                container,
                container.effective_user(),
            ))?,
        );
    }
    if stage.worker.is_some() {
        let requested = components.remove("host").expect("host component");
        components.insert("requested_host".into(), requested);
        components.remove("inherited_environment");
        components.remove("container_runtime");
        let mut evidence = assemble(components)?;
        evidence.format = "preflight-stage-evidence-v2".into();
        evidence.sha256 = digest(&(&evidence.format, &evidence.components))?;
        evidence.reason =
            "remote runtime observation unavailable; remote cache reuse remains disabled".into();
        Ok(evidence)
    } else {
        assemble(components)
    }
}
pub fn explain(db: &Db, query: &Query) -> Result<Value> {
    let current = db.stage_key(&query.run_id, &query.stage_id)?;
    let cache_identity = current
        .as_ref()
        .map(|e| crate::cache::explain_key(db, &query.run_id, e))
        .transpose()?;
    let mut result = json!({"run_id":query.run_id,"stage_id":query.stage_id,"evidence":current,"build_cache_identity":cache_identity});
    use rusqlite::OptionalExtension;
    let remote:Option<(String,String,u64,Option<String>)>=db.connection()?.query_row("SELECT state,worker_id,generation,result_json FROM worker_jobs WHERE run_id=?1 AND stage_id=?2",rusqlite::params![query.run_id,query.stage_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some((state, worker, generation, outcome)) = remote {
        let outcome = outcome
            .map(|v| serde_json::from_str::<crate::worker_jobs::Outcome>(&v))
            .transpose()?;
        result["remote_execution"] = json!({"state":state,"worker_id":worker,"generation":generation,"runtime_receipt":outcome.and_then(|v|v.runtime_receipt)});
    }
    if let Some(baseline) = &query.baseline_id {
        ensure!(
            db.run(&query.run_id)?.project_id == db.run(baseline)?.project_id,
            crate::model::invalid("runs must belong to the same project")
        );
        let previous = db.stage_key(baseline, &query.stage_id)?;
        let previous_cache = previous
            .as_ref()
            .map(|e| crate::cache::explain_key(db, baseline, e))
            .transpose()?;
        let cache_changed = match (&cache_identity, &previous_cache) {
            (Some(a), Some(b)) if a["format"] == b["format"] => Some(
                ["stage_evidence", "source_provenance"]
                    .into_iter()
                    .filter(|k| a["components"][*k] != b["components"][*k])
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        };
        let changed = match (&current, &previous) {
            (Some(a), Some(b)) if a.format == b.format => Some(
                a.components
                    .keys()
                    .chain(b.components.keys())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .filter(|k| a.components.get(*k) != b.components.get(*k))
                    .cloned()
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        };
        result["baseline_id"] = json!(baseline);
        result["baseline"] = json!(previous);
        result["changed_components"] = json!(changed);
        result["baseline_build_cache_identity"] = json!(previous_cache);
        result["changed_cache_components"] = json!(cache_changed);
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_order_and_component_invalidation() {
        let a = assemble(BTreeMap::from([
            ("command".into(), "a".into()),
            ("tree".into(), "b".into()),
        ]))
        .unwrap();
        let b = assemble(BTreeMap::from([
            ("tree".into(), "b".into()),
            ("command".into(), "a".into()),
        ]))
        .unwrap();
        assert_eq!(a.sha256, b.sha256);
        let c = assemble(BTreeMap::from([
            ("command".into(), "changed".into()),
            ("tree".into(), "b".into()),
        ]))
        .unwrap();
        assert_ne!(a.sha256, c.sha256);
        assert!(!a.reusable);
    }
}
