//! Explicit deterministic build reuse; retained producer artifacts remain run-owned.
use crate::{db::Db, model::Artifact, pipeline::StageDefinition};
use anyhow::{Result, ensure};
use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::Path};

pub const FORMAT: &str = "preflight-build-cache-v1";
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default, skip_serializing_if = "is_false")]
    pub single_flight: bool,
    pub pipeline_id: String,
    pub deterministic: bool,
    pub inputs_complete: bool,
}
fn is_false(v: &bool) -> bool {
    !v
}
#[derive(Debug)]
pub struct Candidate {
    pub id: String,
    pub run_id: String,
    pub stage_id: String,
}
/// Only stable provenance fields: never private snapshot or absolute repository paths.
fn provenance(value: &serde_json::Value) -> serde_json::Value {
    let mut result = serde_json::Map::new();
    for key in [
        "format",
        "commit",
        "resolved_commit",
        "patches",
        "index_inventory_sha256",
        "untracked",
        "aggregate_sha256",
        "inclusion_policy",
        "exclusion_policy",
    ] {
        if let Some(v) = value.get(key) {
            result.insert(key.into(), v.clone());
        }
    }
    result.into()
}
pub fn key(db: &Db, run: &str, evidence: &crate::stage_keys::Evidence) -> Result<String> {
    let source = db
        .inputs(run)?
        .and_then(|v| v.source)
        .map(|v| provenance(&v));
    Ok(crate::hashing::hex(Sha256::digest(serde_json::to_vec(&(
        FORMAT,
        &evidence.sha256,
        source,
    ))?)))
}
pub fn restore(
    db: &Db,
    candidate: &Candidate,
    run: &str,
    root: &Path,
    stage: &StageDefinition,
) -> Result<Vec<Artifact>> {
    let _lease = db.begin_operation(
        &candidate.run_id,
        "cache_read",
        BTreeSet::from([format!("cache:{}", candidate.id)]),
    )?;
    let artifacts = db
        .artifacts(&candidate.run_id)?
        .into_iter()
        .filter(|a| a.stage_id == candidate.stage_id)
        .collect::<Vec<_>>();
    restore_outputs(artifacts, run, root, stage)
}
pub(crate) fn restore_outputs(
    artifacts: Vec<Artifact>,
    run: &str,
    root: &Path,
    stage: &StageDefinition,
) -> Result<Vec<Artifact>> {
    ensure!(
        artifacts.len() == stage.outputs.len(),
        "cache output set mismatch"
    );
    let parent = root.join("artifacts");
    std::fs::create_dir_all(&parent)?;
    let temporary = tempfile::tempdir_in(&parent)?;
    let destination = parent.join(&stage.id);
    let mut result = Vec::new();
    for mut artifact in artifacts {
        let spec = stage
            .outputs
            .get(&artifact.name)
            .ok_or_else(|| anyhow::anyhow!("unknown cached output"))?;
        ensure!(artifact.kind == spec.kind, "cache output kind mismatch");
        crate::artifacts::verify_copy(&artifact, &temporary.path().join(&artifact.name))?;
        artifact.run_id = run.into();
        artifact.stage_id = stage.id.clone();
        artifact.path = destination.join(&artifact.name);
        result.push(artifact);
    }
    ensure!(!destination.exists(), "cache destination already exists");
    std::fs::rename(temporary.path(), &destination)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provenance_ignores_locations_but_keeps_patch_and_dirty_identity() {
        let a = serde_json::json!({"format":"git","commit":"a","patches":[{"sha256":"one"}],"snapshot_path":"/run/1","repository_path":"/project/1"});
        let mut b = a.clone();
        b["snapshot_path"] = "/run/2".into();
        b["repository_path"] = "/project/2".into();
        assert_eq!(provenance(&a), provenance(&b));
        b["patches"][0]["sha256"] = "two".into();
        assert_ne!(provenance(&a), provenance(&b));
        assert_ne!(
            provenance(&serde_json::json!({"index_inventory_sha256":"one"})),
            provenance(&serde_json::json!({"index_inventory_sha256":"two"}))
        );
        assert_ne!(
            provenance(&serde_json::json!({"aggregate_sha256":"one"})),
            provenance(&serde_json::json!({"aggregate_sha256":"two"}))
        );
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CleanupPreview {
    pub project_id: String,
    pub pipeline_id: Option<String>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CleanupExecute {
    pub plan_id: String,
}
impl Db {
    pub fn cache_cleanup_preview(&self, request: &CleanupPreview) -> Result<serde_json::Value> {
        use rusqlite::params;
        self.project(&request.project_id)?;
        if let Some(id) = &request.pipeline_id {
            crate::pipeline::identifier(id)?;
        }
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let entries = tx.prepare("SELECT c.id,c.producer_run,c.producer_stage,c.cache_key,EXISTS(SELECT 1 FROM operation_resources r JOIN operation_leases l ON l.id=r.lease_id WHERE l.state IN ('active','abandoned') AND r.resource IN ('cache:'||c.id,'run:'||c.producer_run)) FROM stage_cache c WHERE project_id=?1 AND (?2 IS NULL OR pipeline_id=?2) AND c.state='active' ORDER BY c.id LIMIT 100")?.query_map(params![request.project_id,request.pipeline_id],|r|Ok(serde_json::json!({"id":r.get::<_,String>(0)?,"producer_run":r.get::<_,String>(1)?,"producer_stage":r.get::<_,String>(2)?,"key":r.get::<_,String>(3)?,"protected":r.get::<_,bool>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let total: i64 = tx.query_row("SELECT COUNT(*) FROM stage_cache WHERE project_id=?1 AND (?2 IS NULL OR pipeline_id=?2) AND state='active'",params![request.project_id,request.pipeline_id],|r|r.get(0))?;
        let id = uuid::Uuid::now_v7().to_string();
        let plan = serde_json::json!({"plan_id":id,"project_id":request.project_id,"pipeline_id":request.pipeline_id,"mode":"invalidate_entries","entries":entries,"total":total,"truncated":total>100,"physical_bytes_to_delete":0,"retained":"producer history, artifacts, module publications and consumed copies; execution rechecks active and abandoned leases"});
        tx.execute("INSERT INTO cache_cleanup_plans(id,project_id,pipeline_id,plan_json) VALUES (?1,?2,?3,?4)",params![id,request.project_id,request.pipeline_id,serde_json::to_string(&plan)?])?;
        tx.commit()?;
        Ok(plan)
    }
    pub fn cache_cleanup_execute(&self, request: &CleanupExecute) -> Result<serde_json::Value> {
        use rusqlite::{OptionalExtension, params};
        let mut c = self.connection()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (project,pipeline,plan,result): (String,Option<String>,String,Option<String>) = tx.query_row("SELECT project_id,pipeline_id,plan_json,result_json FROM cache_cleanup_plans WHERE id=?1",[&request.plan_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.ok_or(crate::model::Fault::NotFound)?;
        if let Some(result) = result {
            return Ok(serde_json::from_str(&result)?);
        }
        let plan: serde_json::Value = serde_json::from_str(&plan)?;
        let mut invalidated = Vec::new();
        let mut protected = Vec::new();
        let mut already_inactive = Vec::new();
        for entry in plan["entries"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid persisted cleanup plan"))?
        {
            let id = entry["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid persisted cache id"))?;
            let producer: Option<String> = tx.query_row("SELECT producer_run FROM stage_cache WHERE id=?1 AND project_id=?2 AND (?3 IS NULL OR pipeline_id=?3) AND state='active'",params![id,project,pipeline],|r|r.get(0)).optional()?;
            let Some(producer) = producer else {
                already_inactive.push(id);
                continue;
            };
            let busy: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM operation_resources r JOIN operation_leases l ON l.id=r.lease_id WHERE l.state IN ('active','abandoned') AND r.resource IN (?1,?2))",params![format!("cache:{id}"),format!("run:{producer}")],|r|r.get(0))?;
            if busy {
                protected.push(id);
                continue;
            }
            tx.execute(
                "UPDATE stage_cache SET state='invalidated' WHERE id=?1",
                [id],
            )?;
            invalidated.push(id);
        }
        let result = serde_json::json!({"plan_id":request.plan_id,"project_id":project,"pipeline_id":pipeline,"mode":"invalidate_entries","invalidated":invalidated,"protected":protected,"already_inactive":already_inactive,"physical_bytes_deleted":0,"retry_protected":"create a new preview after operation release; this plan result is immutable"});
        tx.execute(
            "UPDATE cache_cleanup_plans SET result_json=?2 WHERE id=?1",
            params![request.plan_id, serde_json::to_string(&result)?],
        )?;
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use crate::model::{CommandSpec, StartBuild};
    #[test]
    fn cleanup_rechecks_leases_is_scoped_durable_and_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state.sqlite3");
        let db = Db::new(&path).unwrap();
        let project = db.register("p", temp.path()).unwrap();
        db.insert_run(
            "producer",
            &StartBuild {
                project_id: project.id.clone(),
                command: CommandSpec {
                    program: "tool".into(),
                    args: vec![],
                    timeout_seconds: 5,
                },
            },
            temp.path(),
        )
        .unwrap();
        db.cache_publish("producer", "native", "compiler", "key")
            .unwrap();
        db.cache_publish("producer", "native", "other", "key")
            .unwrap();
        let request = CleanupPreview {
            project_id: project.id.clone(),
            pipeline_id: Some("compiler".into()),
        };
        let plan = db.cache_cleanup_preview(&request).unwrap();
        assert_eq!(plan["entries"].as_array().unwrap().len(), 1);
        let entry = plan["entries"][0]["id"].as_str().unwrap();
        assert_eq!(plan["entries"][0]["protected"], false);
        let execute = CleanupExecute {
            plan_id: plan["plan_id"].as_str().unwrap().into(),
        };
        let lease = db
            .begin_operation(
                "producer",
                "cache_read",
                BTreeSet::from([format!("cache:{entry}")]),
            )
            .unwrap();
        let result = db.cache_cleanup_execute(&execute).unwrap();
        assert_eq!(result["protected"], serde_json::json!([entry]));
        drop(lease);
        assert_eq!(result, db.cache_cleanup_execute(&execute).unwrap());
        let next = db.cache_cleanup_preview(&request).unwrap();
        let next_request = CleanupExecute {
            plan_id: next["plan_id"].as_str().unwrap().into(),
        };
        let result = db.cache_cleanup_execute(&next_request).unwrap();
        assert_eq!(result["invalidated"], serde_json::json!([entry]));
        assert!(
            db.begin_operation(
                "producer",
                "cache_read",
                BTreeSet::from([format!("cache:{entry}")])
            )
            .is_err()
        );
        assert_eq!(db.cache_cleanup_preview(&request).unwrap()["total"], 0);
        let other = db
            .cache_cleanup_preview(&CleanupPreview {
                project_id: project.id,
                pipeline_id: None,
            })
            .unwrap();
        assert_eq!(other["total"], 1);
        let orphan = db
            .begin_operation("producer", "orphan", BTreeSet::new())
            .unwrap();
        std::mem::forget(orphan);
        db.reconcile().unwrap();
        let blocked = db
            .cache_cleanup_execute(&CleanupExecute {
                plan_id: other["plan_id"].as_str().unwrap().into(),
            })
            .unwrap();
        assert_eq!(blocked["protected"].as_array().unwrap().len(), 1);
        drop(db);
        let reopened = Db::new(&path).unwrap();
        assert_eq!(
            result,
            reopened.cache_cleanup_execute(&next_request).unwrap()
        );
        assert_eq!(reopened.run("producer").unwrap().id, "producer");
    }
}
